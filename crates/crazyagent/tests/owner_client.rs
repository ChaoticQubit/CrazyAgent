use crazyagent::assets::AssetAction;
use crazyagent::config::resolve_config;
use crazyagent::coordination::OwnerAction;
use crazyagent::engine::owner::*;
use crazyagent::engine::{Client, Engine, Server};
use crazyagent::memory::{HumanReview, KnowledgeAction};
use crazyagent::protocol::*;
use crazyagent::verification::OwnerVerification;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;

fn request(client: &str, operation: &str, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new(client).unwrap(),
        request_id: RequestId::new(operation).unwrap(),
        expected_revision: Cursor::new(0),
        command,
    }
}

fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn baseline() -> crazyagent::verification::BaselineDraft {
    use crazyagent::verification::*;
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("project").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "Exact approved owner checks".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"real code check".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"real integrity check".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

async fn owner_fixture() -> (
    tempfile::TempDir,
    crazyagent::config::EffectiveConfig,
    Arc<Engine>,
) {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-test-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    (root, config, engine)
}

#[tokio::test]
async fn owner_baseline_effect_is_atomic_replayable_and_restart_authenticated() {
    let (root, config, engine) = owner_fixture().await;
    let draft = baseline();
    let id = draft.identity().unwrap();
    assert_eq!(
        engine
            .apply(request(
                "host",
                "project",
                Command::CreateProject {
                    project_id: draft.project_id.clone(),
                    spec: ProjectSpec {
                        name: "Project".into(),
                        repositories: vec![]
                    }
                }
            ))
            .await
            .unwrap()
            .outcome,
        Outcome::Applied
    );
    let mut propose = request(
        "host",
        "draft",
        Command::ProposeBaseline {
            draft: draft.clone(),
        },
    );
    propose.expected_revision = Cursor::new(1);
    assert_eq!(
        engine.apply(propose).await.unwrap().outcome,
        Outcome::Applied
    );
    let (server, host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let old =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let mutation = owner_request(
        "approve",
        2,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ApproveBaseline {
                baseline_id: id.clone(),
            },
        },
    );
    let first = old.request(mutation.clone()).await.unwrap();
    let OwnerResponse::Applied { operation: first } = first.response else {
        panic!("applied")
    };
    assert_eq!(first.result, OwnerResult::Applied);
    assert_eq!(first.provenance.revision_after.value(), 3);
    let detail = old
        .request(owner_request(
            "detail",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Baseline {
                    baseline_id: id.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = detail.response else {
        panic!("read")
    };
    assert!(
        matches!(state.data,OwnerData::Baseline {draft:ref d,approved:true,revoked:false,current:true} if *d==draft)
    );
    let mut changed = mutation.clone();
    changed.expected_revision = Cursor::new(3);
    assert!(
        matches!(old.request(changed).await.unwrap().response,OwnerResponse::Rejected {code} if code=="operation_identity_conflict")
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    drop(host);
    engine.close().await.unwrap();
    drop(engine);
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    assert!(old.request(mutation.clone()).await.is_err());
    let new =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let replay = new.request(mutation).await.unwrap();
    let OwnerResponse::Applied { operation } = replay.response else {
        panic!("replay")
    };
    assert!(operation.historical_replay);
    assert_eq!(operation.provenance, first.provenance);
    let stale = new
        .request(owner_request(
            "stale-revision",
            0,
            OwnerOperation::Apply {
                mutation: OwnerMutation::RevokeBaseline { baseline_id: id },
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(stale.response,OwnerResponse::Applied {operation:OwnerApplied {result:OwnerResult::Rejected {code},..}} if code=="stale_revision")
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    drop(host);
    engine.close().await.unwrap();
    drop(engine);
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE event_cursor IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[tokio::test]
async fn owner_discovery_is_keyset_bounded_and_config_revision_pinned() {
    let (root, config, engine) = owner_fixture().await;
    for n in 0..3 {
        let mut r = request(
            "host",
            &format!("project-{n}"),
            Command::CreateProject {
                project_id: ProjectId::new(format!("p{n}")).unwrap(),
                spec: ProjectSpec {
                    name: format!("Project {n}"),
                    repositories: vec![],
                },
            },
        );
        r.expected_revision = Cursor::new(n);
        assert_eq!(engine.apply(r).await.unwrap().outcome, Outcome::Applied);
    }
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let page = client
        .request(owner_request(
            "page",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Projects,
                    cursor: None,
                    limit: 2,
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = page.response else {
        panic!("page")
    };
    let OwnerData::Page {
        items,
        next: Some(cursor),
    } = state.data
    else {
        panic!("continuation")
    };
    assert_eq!(items.len(), 2);
    let rest = client
        .request(owner_request(
            "rest",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Projects,
                    cursor: Some(cursor.clone()),
                    limit: 2,
                },
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(rest.response,OwnerResponse::Read {state:OwnerReadReply {data:OwnerData::Page {items,next:None},..}} if items.len()==1)
    );
    let desktop = OwnerClient::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .unwrap();
    let wrong = desktop
        .request(owner_request(
            "foreign",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Projects,
                    cursor: Some(cursor.clone()),
                    limit: 2,
                },
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        wrong.response,
        OwnerResponse::Read {
            state: OwnerReadReply {
                data: OwnerData::ResyncRequired,
                ..
            }
        }
    ));
    std::fs::write(
        root.path().join("g"),
        format!(
            "version=1\nevents_retained={}\n",
            config.settings.events_retained + 1
        ),
    )
    .unwrap();
    let next = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    engine.update_config(next).await.unwrap();
    let stale = client
        .request(owner_request(
            "config-changed",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Projects,
                    cursor: Some(cursor),
                    limit: 2,
                },
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        stale.response,
        OwnerResponse::Read {
            state: OwnerReadReply {
                data: OwnerData::ResyncRequired,
                ..
            }
        }
    ));
    for limit in [0, 33] {
        assert!(
            client
                .request(owner_request(
                    "invalid",
                    0,
                    OwnerOperation::Read {
                        query: OwnerRead::Page {
                            view: OwnerView::Projects,
                            cursor: None,
                            limit
                        }
                    }
                ))
                .await
                .is_err()
        );
    }
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_bootstrap_refuses_unknown_symlink_hardlink_and_replacement() {
    let (root, config, engine) = owner_fixture().await;
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"preserve").unwrap();
    std::os::unix::fs::symlink(&foreign, root.path().join("owner-tui.cap")).unwrap();
    assert!(
        OwnerServer::bind(root.path(), engine.clone(), &config)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&foreign).unwrap(), b"preserve");
    std::fs::remove_file(root.path().join("owner-tui.cap")).unwrap();
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    std::fs::hard_link(
        root.path().join("owner-tui.cap"),
        root.path().join("extra-link"),
    )
    .unwrap();
    assert!(
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).is_err()
    );
    assert!(
        client
            .request(owner_request(
                "hardlink",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    std::fs::remove_file(root.path().join("extra-link")).unwrap();
    std::fs::rename(
        root.path().join("owner-tui.cap"),
        root.path().join("known-original"),
    )
    .unwrap();
    std::fs::write(root.path().join("owner-tui.cap"), b"preserve replacement").unwrap();
    assert!(
        client
            .request(owner_request(
                "replaced",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    stop.send(()).unwrap();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        std::fs::read(root.path().join("owner-tui.cap")).unwrap(),
        b"preserve replacement"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_rejects_observed_credential_content_replacement() {
    let (root, config, engine) = owner_fixture().await;
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let path = root.path().join("owner-tui.cap");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(
        client
            .request(owner_request(
                "observed-content-change",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_missing_wrong_partial_and_oversized_frames_never_gain_authority() {
    let (root, _config, engine) = owner_fixture().await;
    let config_path = root.path().join("short-timeout");
    std::fs::write(&config_path, "version=1\ntimeout_ms=100\n").unwrap();
    let config = resolve_config(&config_path, &root.path().join("p")).unwrap();
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let mut absent = UnixStream::connect(root.path().join("owner.sock"))
        .await
        .unwrap();
    absent
        .write_all(&encode_frame(&request("worker", "worker-frame", Command::Snapshot)).unwrap())
        .await
        .unwrap();
    let mut byte = [0; 1];
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), absent.read(&mut byte)).await,
        Ok(Ok(0)) | Ok(Err(_))
    ));
    let mut wrong = UnixStream::connect(root.path().join("owner.sock"))
        .await
        .unwrap();
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(b"CAO1");
    packet[4] = 0;
    wrong.write_all(&packet).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), wrong.read(&mut byte)).await,
        Ok(Ok(0)) | Ok(Err(_))
    ));
    let token = std::fs::read(root.path().join("owner-tui.cap")).unwrap();
    packet[5..].copy_from_slice(&token);
    for bytes in [
        vec![0, 0],
        ((FRAME_BYTES_MAX + 1) as u32).to_be_bytes().to_vec(),
    ] {
        let mut stream = UnixStream::connect(root.path().join("owner.sock"))
            .await
            .unwrap();
        stream.write_all(&packet).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte)).await,
            Ok(Ok(0)) | Ok(Err(_))
        ));
    }
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(1)).unwrap();
    let good = client
        .request(owner_request(
            "healthy",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(good.response,OwnerResponse::Read {state:OwnerReadReply {revision,..}} if revision.value()==0)
    );
    let unsupported = serde_json::json!({"protocol_version":1,"request_id":"forged","expected_revision":"0","operation":{"kind":"Apply","mutation":{"kind":"Register","id":"forged"}}});
    assert!(serde_json::from_value::<OwnerRequest>(unsupported).is_err());
    let mut forged = serde_json::to_value(owner_request(
        "forged",
        0,
        OwnerOperation::Read {
            query: OwnerRead::Capabilities,
        },
    ))
    .unwrap();
    forged["principal"] = "owner".into();
    assert!(serde_json::from_value::<OwnerRequest>(forged).is_err());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_root_and_socket_replacement_preserve_unknown_artifacts() {
    let parent = tempfile::tempdir_in("/private/tmp").unwrap();
    let runtime = parent.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&runtime.join("g"), &runtime.join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&runtime.join("db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, _host) = OwnerServer::bind(&runtime, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(&runtime, OwnerClientKind::Tui, Duration::from_secs(1)).unwrap();
    std::fs::rename(&runtime, parent.path().join("original")).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        runtime.join("owner.sock"),
        b"preserve unknown socket replacement",
    )
    .unwrap();
    assert!(
        client
            .request(owner_request(
                "root-replaced",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    stop.send(()).unwrap();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        std::fs::read(runtime.join("owner.sock")).unwrap(),
        b"preserve unknown socket replacement"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn readonly_actor_does_not_install_owner_schema_or_publish_capabilities() {
    let (root, config, engine) = owner_fixture().await;
    engine.close().await.unwrap();
    drop(engine);
    let readonly = Arc::new(
        Engine::open_readonly(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    assert!(
        OwnerServer::bind(root.path(), readonly.clone(), &config)
            .await
            .is_err()
    );
    assert!(!root.path().join("owner-tui.cap").exists());
    assert!(!root.path().join("owner.sock").exists());
    readonly.close().await.unwrap();
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name GLOB 'owner_*'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn owner_candidate_discovery_reports_actual_unavailable_verification() {
    use crazyagent::verification::CandidateSnapshot;
    let (root, config, engine) = owner_fixture().await;
    let mut revision = 0;
    let draft = baseline();
    let baseline_id = draft.identity().unwrap();
    let project = draft.project_id.clone();
    host_command(
        &engine,
        &mut revision,
        "project",
        Command::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "Project".into(),
                repositories: vec![],
            },
        },
    )
    .await;
    host_command(
        &engine,
        &mut revision,
        "draft",
        Command::ProposeBaseline { draft },
    )
    .await;
    host_command(
        &engine,
        &mut revision,
        "approve",
        Command::OwnerVerification {
            action: OwnerVerification::Approve {
                baseline_id: baseline_id.clone(),
            },
        },
    )
    .await;
    let snapshot = CandidateSnapshot {
        files: std::collections::BTreeMap::from([("src/main.rs".into(), b"fn main() {}".to_vec())]),
        head_digest: "a".repeat(64),
        target_digest: "b".repeat(64),
    };
    let context = ExecutionContext {
        project_id: project,
        task_id: TaskId::new("task").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: snapshot.identity().unwrap(),
        baseline_id,
        allowed_destinations: vec![],
        budget_ref: CredentialRef::new("budget").unwrap(),
    };
    host_command(
        &engine,
        &mut revision,
        "pin",
        Command::PinTask {
            context: context.clone(),
        },
    )
    .await;
    let candidate_id = RequestId::new("candidate").unwrap();
    host_command(
        &engine,
        &mut revision,
        "register",
        Command::OwnerVerification {
            action: OwnerVerification::Register {
                id: candidate_id.clone(),
                task_id: context.task_id,
                snapshot,
                environment_id: EnvironmentId::new("environment").unwrap(),
            },
        },
    )
    .await;
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    for view in [
        OwnerView::Candidates,
        OwnerView::Tasks,
        OwnerView::Baselines,
    ] {
        let page = client
            .request(owner_request(
                "page",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Page {
                        view,
                        cursor: None,
                        limit: 2,
                    },
                },
            ))
            .await
            .unwrap();
        assert!(
            matches!(page.response,OwnerResponse::Read {state:OwnerReadReply {data:OwnerData::Page {items,next:None},..}} if items.len()==1)
        );
    }
    let inspected = client
        .request(owner_request(
            "inspect",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Inspect {
                    candidate_id: candidate_id.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = inspected.response else {
        panic!("read")
    };
    assert!(
        matches!(state.data,OwnerData::Domain {outcome} if matches!(&*outcome,Outcome::Verification {state} if state.candidate.id==candidate_id && state.attempts.is_empty()))
    );
    let full = client
        .request(owner_request(
            "full-suite",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::FullSuite {
                    candidate_id: candidate_id.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = full.response else {
        panic!("full-suite")
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    revision = operation.current_revision.value();
    let verify = client
        .request(owner_request(
            "verify-unavailable",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::Verify {
                    candidate_id: candidate_id.clone(),
                    attempt_id: RequestId::new("attempt").unwrap(),
                },
            },
        ))
        .await
        .unwrap();
    let inspected = client
        .request(owner_request(
            "actual-attempt",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Inspect { candidate_id },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = inspected.response else {
        panic!("read")
    };
    let OwnerData::Domain { outcome } = state.data else {
        panic!("domain")
    };
    let Outcome::Verification { state } = *outcome else {
        panic!("verification")
    };
    assert_eq!(state.attempts.len(), 1);
    assert!(!state.attempts[0].1.outcomes.is_empty());
    assert!(
        state.attempts[0]
            .1
            .outcomes
            .values()
            .all(|outcome| *outcome == crazyagent::verification::CheckOutcome::Unavailable)
    );
    assert!(state.attempts[0].1.accepts(&state.current).is_err());
    assert!(
        matches!(verify.response,OwnerResponse::Applied {operation:OwnerApplied {result:OwnerResult::VerificationUnavailable {attempt_id},..}} if attempt_id.as_str()=="attempt")
    );
    let token = std::fs::read(root.path().join("owner-tui.cap")).unwrap();
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
    drop(engine);
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    for sql in [
        "SELECT request FROM operations",
        "SELECT reply FROM operations",
        "SELECT result FROM owner_operations",
        "SELECT body FROM events",
    ] {
        let mut stmt = db.prepare(sql).unwrap();
        let rows = stmt
            .query_map([], |r| Ok(r.get_ref(0)?.as_bytes()?.to_vec()))
            .unwrap();
        for row in rows {
            assert!(
                !row.unwrap()
                    .windows(token.len())
                    .any(|bytes| bytes == token.as_slice())
            );
        }
    }
}

async fn host_command(engine: &Engine, revision: &mut u64, id: &str, command: Command) {
    let mut r = request("host", id, command);
    r.expected_revision = Cursor::new(*revision);
    let reply = engine.apply(r).await.unwrap();
    assert_eq!(reply.outcome, Outcome::Applied);
    *revision = reply.revision.value();
}

#[tokio::test]
async fn owner_knowledge_reviews_exact_versions_and_reevaluates_source_revocation() {
    use crazyagent::memory::*;
    let (root, config, engine) = owner_fixture().await;
    let mut revision = 0;
    let project = ProjectId::new("p").unwrap();
    host_command(
        &engine,
        &mut revision,
        "project",
        Command::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "Project".into(),
                repositories: vec![],
            },
        },
    )
    .await;
    for (name, role) in [
        ("writer", KnowledgeRole::Contributor),
        ("curator", KnowledgeRole::Curator),
    ] {
        let task = TaskId::new(name).unwrap();
        host_command(
            &engine,
            &mut revision,
            &format!("pin-{name}"),
            Command::PinTask {
                context: ExecutionContext {
                    project_id: project.clone(),
                    task_id: task.clone(),
                    config_id: config.id.clone(),
                    snapshot_id: SnapshotId::new("s").unwrap(),
                    baseline_id: BaselineId::new("b").unwrap(),
                    allowed_destinations: vec![],
                    budget_ref: CredentialRef::new("budget").unwrap(),
                },
            },
        )
        .await;
        host_command(
            &engine,
            &mut revision,
            &format!("admit-{name}"),
            Command::Knowledge {
                action: KnowledgeAction::Admit {
                    principal: Principal {
                        id: RequestId::new(name).unwrap(),
                        project_id: project.clone(),
                        task_id: task,
                        session_id: SessionId::new(name).unwrap(),
                        role,
                    },
                },
            },
        )
        .await;
    }
    host_command(
        &engine,
        &mut revision,
        "capture",
        Command::Knowledge {
            action: KnowledgeAction::Capture {
                principal_id: RequestId::new("writer").unwrap(),
                activity: ActivityInput {
                    id: RequestId::new("activity").unwrap(),
                    source_id: RequestId::new("source").unwrap(),
                    category: CaptureCategory::ToolOutput,
                    text: "Observed local build fact".into(),
                    gaps: vec![],
                },
            },
        },
    )
    .await;
    host_command(
        &engine,
        &mut revision,
        "nominate",
        Command::Knowledge {
            action: KnowledgeAction::Nominate {
                principal_id: RequestId::new("writer").unwrap(),
                nomination: NominationInput {
                    id: RequestId::new("nomination").unwrap(),
                    activity_ids: vec![RequestId::new("activity").unwrap()],
                    text: "Observed build fact".into(),
                },
            },
        },
    )
    .await;
    host_command(
        &engine,
        &mut revision,
        "publish",
        Command::Knowledge {
            action: KnowledgeAction::Publish {
                principal_id: RequestId::new("curator").unwrap(),
                nomination_id: RequestId::new("nomination").unwrap(),
                entry_id: RequestId::new("entry").unwrap(),
                text: "Curated build fact".into(),
            },
        },
    )
    .await;
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let tui =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let desktop = OwnerClient::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .unwrap();
    let inbox = owner_request(
        "inbox",
        0,
        OwnerOperation::Read {
            query: OwnerRead::Knowledge {
                review: HumanReview::Inbox {
                    project_id: project.clone(),
                    after: None,
                },
            },
        },
    );
    let response = tui.request(inbox.clone()).await.unwrap();
    let OwnerResponse::Read { state } = response.response else {
        panic!("read")
    };
    let OwnerData::Domain { outcome } = state.data else {
        panic!("domain")
    };
    let Outcome::Knowledge { entries } = *outcome else {
        panic!("entries")
    };
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    let stale = desktop
        .request(owner_request(
            "stale-version",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::Knowledge {
                    review: HumanReview::Review {
                        entry_id: entry.id.clone(),
                        version: entry.version + 1,
                        decision: ReviewDecision::Keep,
                    },
                },
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        stale.response,
        OwnerResponse::Applied {
            operation: OwnerApplied {
                result: OwnerResult::Rejected { .. },
                ..
            }
        }
    ));
    let keep = tui
        .request(owner_request(
            "keep",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::Knowledge {
                    review: HumanReview::Review {
                        entry_id: entry.id.clone(),
                        version: entry.version,
                        decision: ReviewDecision::Keep,
                    },
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = keep.response else {
        panic!("applied")
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    revision = operation.current_revision.value();
    let revoke = desktop
        .request(owner_request(
            "source-revoke",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::Knowledge {
                    review: HumanReview::RevokeSource {
                        project_id: project,
                        source_id: RequestId::new("source").unwrap(),
                    },
                },
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        revoke.response,
        OwnerResponse::Applied {
            operation: OwnerApplied {
                result: OwnerResult::Applied,
                ..
            }
        }
    ));
    let after = tui.request(inbox).await.unwrap();
    let OwnerResponse::Read { state } = after.response else {
        panic!("read")
    };
    let OwnerData::Domain { outcome } = state.data else {
        panic!("domain")
    };
    let Outcome::Knowledge { entries: after } = *outcome else {
        panic!("entries")
    };
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].status, KnowledgeStatus::Retracted);
    assert_eq!(after[0].version, entry.version + 2);
    assert_eq!(after[0].content_hash, entry.content_hash);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_approves_real_coordination_plan_and_discovers_it_without_launch_claims() {
    use crazyagent::coordination::{PlanAuthority, Role, WorkItem, WorkPlan};
    use crazyagent::routing::{ResourceBudget, RouteRequest};
    let (root, _config, engine) = owner_fixture().await;
    std::fs::write(
        root.path().join("g"),
        "[routing]\napproved_accounts=['account']\n",
    )
    .unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    engine.update_config(config.clone()).await.unwrap();
    let mut revision = 0;
    let project = ProjectId::new("p").unwrap();
    host_command(
        &engine,
        &mut revision,
        "project",
        Command::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "Project".into(),
                repositories: vec![],
            },
        },
    )
    .await;
    let context = ExecutionContext {
        project_id: project,
        task_id: TaskId::new("task").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: SnapshotId::new("s").unwrap(),
        baseline_id: BaselineId::new("b").unwrap(),
        allowed_destinations: vec![],
        budget_ref: CredentialRef::new("budget").unwrap(),
    };
    host_command(
        &engine,
        &mut revision,
        "pin",
        Command::PinTask {
            context: context.clone(),
        },
    )
    .await;
    let plan = WorkPlan {
        id: PlanId::new("plan").unwrap(),
        config_id: config.id.clone(),
        scope: context,
        authority: PlanAuthority::Human,
        dependencies: vec![],
        tasks: vec![WorkItem {
            id: TaskId::new("item").unwrap(),
            session_id: SessionId::new("worker").unwrap(),
            role: Role::Worker,
            native_descendant: None,
            route: RouteRequest {
                resources: ResourceBudget::unit(),
                preferred_target: None,
                required_target: None,
                required_capabilities: vec![],
                account_ref: CredentialRef::new("account").unwrap(),
                budget_ref: CredentialRef::new("budget").unwrap(),
            },
        }],
    };
    host_command(
        &engine,
        &mut revision,
        "propose",
        Command::ProposePlan { plan: plan.clone() },
    )
    .await;
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client = OwnerClient::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .unwrap();
    let approved = client
        .request(owner_request(
            "approve",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::Coordination {
                    action: OwnerAction::ApprovePlan {
                        plan_id: plan.id.clone(),
                    },
                },
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        approved.response,
        OwnerResponse::Applied {
            operation: OwnerApplied {
                result: OwnerResult::Applied,
                ..
            }
        }
    ));
    let state = client
        .request(owner_request(
            "state",
            0,
            OwnerOperation::Read {
                query: OwnerRead::CoordinationState {
                    plan_id: plan.id.clone(),
                },
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = state.response else {
        panic!("read")
    };
    assert!(
        matches!(state.data,OwnerData::Domain {outcome} if matches!(&*outcome,Outcome::Coordination {state} if state.approved && state.reservations.is_empty()))
    );
    let page = client
        .request(owner_request(
            "plans",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Page {
                    view: OwnerView::Plans,
                    cursor: None,
                    limit: 16,
                },
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(page.response,OwnerResponse::Read {state:OwnerReadReply {data:OwnerData::Page {items,next:None},..}} if items.len()==1)
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn owner_socket_authenticates_reconnects_and_reconciles_exact_mutation() {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-live-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(server.run(rx));
    let client =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let desktop = OwnerClient::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .unwrap();
    let req = owner_request(
        "missing-plan",
        0,
        OwnerOperation::Apply {
            mutation: OwnerMutation::Coordination {
                action: OwnerAction::ApprovePlan {
                    plan_id: PlanId::new("missing").unwrap(),
                },
            },
        },
    );
    let reply = client.request(req.clone()).await.unwrap();
    let OwnerResponse::Applied { operation: first } = reply.response else {
        panic!("expected durable domain result")
    };
    assert!(matches!(first.result, OwnerResult::Rejected { .. }));
    drop(client);
    let again =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let replay = again.request(req.clone()).await.unwrap();
    let OwnerResponse::Applied { operation } = replay.response else {
        panic!("replay")
    };
    assert!(operation.historical_replay);
    assert_eq!(operation.provenance, first.provenance);
    let recovered = again
        .request(owner_request(
            "reconcile",
            0,
            OwnerOperation::Reconcile {
                operation_id: req.request_id.clone(),
                expected_hash: Some(req.mutation_hash().unwrap()),
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        recovered.response,
        OwnerResponse::Committed { .. }
    ));
    let other = desktop
        .request(owner_request(
            "other-owner",
            0,
            OwnerOperation::Reconcile {
                operation_id: req.request_id,
                expected_hash: None,
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        other.response,
        OwnerResponse::NotRecordedAtBarrier { .. }
    ));
    let epoch = host.epoch(OwnerClientKind::Tui).unwrap();
    let revoked = host
        .control(OwnerControl {
            operation_id: RequestId::new("revoke").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: epoch,
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    assert!(!revoked.active);
    assert!(
        again
            .request(owner_request(
                "revoked",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    let active = host
        .control(OwnerControl {
            operation_id: RequestId::new("reactivate").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: revoked.owner_epoch,
            decision: OwnerDecision::Reactivate,
        })
        .await
        .unwrap();
    assert!(active.active);
    assert!(
        again
            .request(owner_request(
                "old-token",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .is_err()
    );
    let fresh =
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    assert!(matches!(
        fresh
            .request(owner_request(
                "fresh",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::Capabilities
                }
            ))
            .await
            .unwrap()
            .response,
        OwnerResponse::Read { .. }
    ));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn worker_socket_rejects_owner_namespace_before_mutation() {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("global"), &root.path().join("project")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("engine.db"), config.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("worker.sock");
    let server = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.run(stopped));
    let client = Client::new(socket.clone(), Duration::from_secs(3)).unwrap();
    let before = client
        .request(request("worker", "before", Command::Snapshot))
        .await
        .unwrap();
    let forged = request(
        "owner:tui",
        "reserved-operation",
        Command::CreateProject {
            project_id: ProjectId::new("unauthorized-project").unwrap(),
            spec: ProjectSpec {
                name: "Unauthorized owner namespace".into(),
                repositories: Vec::new(),
            },
        },
    );
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(&encode_frame(&forged).unwrap())
        .await
        .unwrap();
    let mut first_byte = [0u8; 1];
    let received = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut first_byte)).await;
    drop(stream);
    let after = client
        .request(request("worker", "after", Command::Snapshot))
        .await
        .unwrap();
    stop.send(()).unwrap();
    server_task.await.unwrap().unwrap();
    engine.close().await.unwrap();
    assert!(
        matches!(before.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
    assert!(matches!(received, Ok(Ok(0)) | Ok(Err(_))));
    assert_eq!(after.revision, Cursor::new(0));
    assert!(
        matches!(after.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
}

#[test]
fn worker_decoder_preserves_every_privileged_command_denial() {
    let commands = [
        Command::OwnerVerification {
            action: OwnerVerification::Revoke {
                baseline_id: BaselineId::new("baseline").unwrap(),
            },
        },
        Command::Knowledge {
            action: KnowledgeAction::Owner {
                action: HumanReview::Inbox {
                    project_id: ProjectId::new("project").unwrap(),
                    after: None,
                },
            },
        },
        Command::Assets {
            action: AssetAction::Revoke {
                source_id: RequestId::new("source").unwrap(),
            },
        },
        Command::OwnerCoordination {
            action: OwnerAction::ApprovePlan {
                plan_id: PlanId::new("plan").unwrap(),
            },
        },
    ];
    for command in commands {
        let frame = encode_frame(&request("worker", "forged-owner", command)).unwrap();
        assert!(decode_request(&frame).is_err());
    }
}

#[tokio::test]
async fn legacy_actor_rejects_owner_namespace_before_mutation() {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("global"), &root.path().join("project")).unwrap();
    let engine = Engine::open(&root.path().join("engine.db"), config)
        .await
        .unwrap();
    let attempted = engine
        .apply(request(
            "owner:tui",
            "legacy-owner-spoof",
            Command::CreateProject {
                project_id: ProjectId::new("legacy-owner-project").unwrap(),
                spec: ProjectSpec {
                    name: "Legacy owner spoof".into(),
                    repositories: Vec::new(),
                },
            },
        ))
        .await;
    let snapshot = engine
        .apply(request("worker", "after-legacy-spoof", Command::Snapshot))
        .await
        .unwrap();
    engine.close().await.unwrap();
    assert!(attempted.is_err());
    assert_eq!(snapshot.revision, Cursor::new(0));
    assert!(
        matches!(snapshot.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
}
