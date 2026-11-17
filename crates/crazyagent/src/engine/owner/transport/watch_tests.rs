use super::*;
use crate::protocol::Topic;
use crate::questions::{self, Ask, Asker, QuestionKind, Settlement};
use serde_json::json;
use std::time::Instant as Clock;

/// How soon a commit must wake a watch (v2.5 plan row S3a).
const WAKE: Duration = Duration::from_secs(1);

/// An engine root whose store holds the engine, team and questions tables and one set-up
/// project, as the questions transport rows build it. Output: the temporary folder, the
/// store's path and the project.
fn store() -> (tempfile::TempDir, PathBuf, ProjectId) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-watch-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::create_dir(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    let db = questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::team::initialize(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, "alpha", &folder);
    (tmp, root.join(DATABASE), ProjectId::new("alpha").unwrap())
}

/// A watch request. Inputs: the cursor, the mark and the wait in ms. Output: the request.
fn request(after: u64, mark: Option<u64>, wait_ms: u32) -> Request {
    Request {
        after: Cursor::new(after),
        mark,
        wait_ms,
    }
}

/// The mark of a reply that is not a refusal. Input: the reply. Output: the mark.
fn mark_of(reply: &WatchReply) -> u64 {
    match reply {
        WatchReply::Changed { mark, .. }
        | WatchReply::Refresh { mark, .. }
        | WatchReply::Quiet { mark, .. } => *mark,
        WatchReply::Rejected { code } => panic!("refused: {code}"),
    }
}

/// Watches once with no mark, as a client's first watch does. Inputs: the watches and the
/// store. Output: the mark of the reply.
async fn first(watches: &Watches, database: &Path) -> u64 {
    let reply = wait(watches, database, request(0, None, 5_000))
        .await
        .unwrap();
    mark_of(&reply)
}

/// Starts a watch in its own task, as a held owner connection waits. Inputs: the watches,
/// the store, the cursor and the mark. Output: the task.
fn waiting(
    watches: &Arc<Watches>,
    database: &Path,
    after: u64,
    mark: u64,
) -> tokio::task::JoinHandle<Result<WatchReply, EngineError>> {
    let (watches, database) = (Arc::clone(watches), database.to_path_buf());
    tokio::spawn(async move { wait(&watches, &database, request(after, Some(mark), 10_000)).await })
}

#[test]
fn a_watch_body_selects_the_watch_protocol_is_bounded_and_is_refused_in_its_shape() {
    let body = |wait_ms: u32| {
        serde_json::to_vec(&json!({"watch": {"after": "0", "mark": null, "wait_ms": wait_ms}}))
            .unwrap()
    };
    assert!(matches!(protocol(&body(1_000)).unwrap(), Protocol::Watch));
    assert_eq!(parse(&body(WAIT_MS_MAX)).unwrap().wait_ms, WAIT_MS_MAX);
    let pad = "x".repeat(REQUEST_BYTES_MAX);
    let long = json!({"watch": {"after": "0", "mark": null, "wait_ms": 1, "pad": pad}});
    for refused in [
        body(WAIT_MS_MAX + 1),
        br#"{"watch": {"after": "0", "mark": null, "wait_ms": 1, "more": 1}}"#.to_vec(),
        serde_json::to_vec(&long).unwrap(),
    ] {
        assert_eq!(
            parse(&refused).err(),
            Some(failure("watch_request_invalid"))
        );
    }
    let frame = super::super::refusal(false, &body(1_000), "owner_busy")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<WatchReply>(&frame[4..]).unwrap(),
        WatchReply::Rejected {
            code: "owner_busy".into()
        }
    );
}

#[tokio::test]
async fn a_first_watch_or_one_with_another_mark_returns_at_once_and_a_quiet_one_at_its_wait() {
    let (_tmp, database, _) = store();
    let watches = Watches::new();
    let started = Clock::now();
    let reply = wait(&watches, &database, request(0, None, 5_000))
        .await
        .unwrap();
    let mark = mark_of(&reply);
    assert_eq!(
        reply,
        WatchReply::Changed {
            cursor: Cursor::new(0),
            mark,
            topics: vec![Topic::Unknown],
        }
    );
    assert_eq!(
        wait(
            &watches,
            &database,
            request(0, Some(mark.wrapping_add(1)), 5_000)
        )
        .await
        .unwrap(),
        WatchReply::Changed {
            cursor: Cursor::new(0),
            mark,
            topics: vec![Topic::Unknown],
        },
        "a mark from another engine or an older sampler"
    );
    assert!(started.elapsed() < WAKE);
    let started = Clock::now();
    assert_eq!(
        wait(&watches, &database, request(0, Some(mark), 300))
            .await
            .unwrap(),
        WatchReply::Quiet {
            cursor: Cursor::new(0),
            mark,
        }
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn a_commit_on_another_connection_wakes_a_watch_within_a_second_as_unknown() {
    let (_tmp, database, project) = store();
    let db = questions::open(database.parent().unwrap()).unwrap();
    let ask = Ask {
        question: "Ship on Friday?".into(),
        kind: QuestionKind::Single,
        options: vec!["Yes".into(), "No".into()],
        recommended: Some("Yes".into()),
        context: None,
    };
    let id = questions::ask(&db, &project, &Asker::Master(41), &ask).unwrap();
    let watches = Arc::new(Watches::new());
    let mark = first(&watches, &database).await;
    let watch = waiting(&watches, &database, 0, mark);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!watch.is_finished(), "nothing committed yet");
    let dismissed = Clock::now();
    assert_eq!(questions::dismiss(&db, id, 0).unwrap(), Settlement::Settled);
    let reply = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(dismissed.elapsed() < WAKE, "{:?}", dismissed.elapsed());
    let WatchReply::Changed {
        cursor,
        mark: moved,
        topics,
    } = reply
    else {
        panic!("expected a change, got {reply:?}");
    };
    assert_eq!((cursor, topics), (Cursor::new(0), vec![Topic::Unknown]));
    assert_ne!(moved, mark);
}

#[tokio::test]
async fn an_owner_request_wakes_a_watch_with_its_topic_and_the_next_watch_waits() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let database = root.path().join(DATABASE);
    let engine = Engine::open(&database, config).await.unwrap();
    let watches = Arc::new(Watches::new());
    let mark = first(&watches, &database).await;
    let watch = waiting(&watches, &database, 0, mark);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!watch.is_finished(), "nothing committed yet");
    let applied = Clock::now();
    engine
        .apply(WireRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            client_id: ClientId::new("desktop").unwrap(),
            request_id: RequestId::new("create-1").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("project-1").unwrap(),
                spec: ProjectSpec {
                    name: "Inventory".into(),
                    repositories: vec!["/tmp/inventory".into()],
                },
            },
        })
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(applied.elapsed() < WAKE, "{:?}", applied.elapsed());
    let moved = mark_of(&reply);
    assert_eq!(
        reply,
        WatchReply::Changed {
            cursor: Cursor::new(1),
            mark: moved,
            topics: vec![Topic::Projects, Topic::Unknown],
        }
    );
    assert_eq!(
        wait(&watches, &database, request(1, Some(moved), 300))
            .await
            .unwrap(),
        WatchReply::Quiet {
            cursor: Cursor::new(1),
            mark: moved,
        }
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn a_cursor_pruned_below_the_retained_events_or_ahead_gets_a_full_refresh() {
    let (_tmp, database, _) = store();
    let db = questions::open(database.parent().unwrap()).unwrap();
    for cursor in [4_u64, 5] {
        let event = crate::protocol::Event {
            cursor: Cursor::new(cursor),
            request_id: RequestId::new(format!("r{cursor}")).unwrap(),
            client_id: ClientId::new("desktop").unwrap(),
            topic: Topic::Team,
        };
        db.execute(
            "INSERT INTO events VALUES (?1,?2)",
            rusqlite::params![cursor as i64, serde_json::to_string(&event).unwrap()],
        )
        .unwrap();
    }
    db.execute("UPDATE state SET revision=5 WHERE id=1", [])
        .unwrap();
    let watches = Watches::new();
    for after in [1, 9] {
        let reply = wait(&watches, &database, request(after, None, 5_000))
            .await
            .unwrap();
        assert_eq!(
            reply,
            WatchReply::Refresh {
                cursor: Cursor::new(5),
                mark: mark_of(&reply),
            },
            "after {after}"
        );
    }
}

#[tokio::test]
async fn past_its_bound_a_watch_is_refused_busy_until_one_ends() {
    let (_tmp, database, _) = store();
    let watches = Watches::new();
    let mut held: Vec<_> = (0..WATCHES_MAX)
        .map(|_| Waiting::take(&watches.waiting).unwrap())
        .collect();
    assert_eq!(
        wait(&watches, &database, request(0, None, 1_000)).await,
        Err(failure("watch_busy"))
    );
    held.pop();
    assert!(
        wait(&watches, &database, request(0, None, 1_000))
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_revoked_while_it_watches_is_refused_and_its_watching_ends() {
    let owner = super::super::usage_tests::Usage::start().await;
    let first = owner
        .client
        .watch(Cursor::new(0), None, 1_000)
        .await
        .unwrap();
    let WatchReply::Changed { cursor, mark, .. } = first else {
        panic!("a first watch returns at once, got {first:?}");
    };
    let client = OwnerClient::attach(
        &owner.client.root.path,
        OwnerClientKind::Tui,
        Duration::from_secs(20),
    )
    .unwrap();
    let watch = tokio::spawn(async move {
        let (mut cursor, mut mark) = (cursor, mark);
        loop {
            match client.watch(cursor.clone(), Some(mark), 10_000).await {
                Ok(
                    WatchReply::Changed {
                        cursor: next,
                        mark: moved,
                        ..
                    }
                    | WatchReply::Refresh {
                        cursor: next,
                        mark: moved,
                    }
                    | WatchReply::Quiet {
                        cursor: next,
                        mark: moved,
                    },
                ) => (cursor, mark) = (next, moved),
                other => return other,
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!watch.is_finished(), "an admitted owner keeps watching");
    let runtime = owner.runtime();
    runtime
        .control(OwnerControl {
            operation_id: RequestId::new("revoke-watch").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .unwrap()
        .unwrap();
    let refused = reply.expect_err("a revoked owner gets no reply");
    assert!(
        refused == failure("owner_grant_stale_or_revoked")
            || refused == failure("owner_authentication_failed"),
        "{refused:?}"
    );
    owner.stop().await;
}
