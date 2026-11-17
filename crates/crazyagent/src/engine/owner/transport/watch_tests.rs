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

/// A revoked owner gets no reply. A watch waiting at the revoke is refused at its second
/// check with the handshake's code, `owner_authentication_failed`, or
/// `owner_grant_stale_or_revoked`; a re-watch after it is refused at admission, which answers
/// nothing, so the client reads `watch_outcome_unknown`, or `watch_unavailable` when the
/// engine closed first. A fresh watch after the revoke ends at admission too.
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
        let reply = loop {
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
                other => break other,
            }
        };
        (reply, client)
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
    let (reply, client) = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .unwrap()
        .unwrap();
    let unanswered = ["watch_outcome_unknown", "watch_unavailable"];
    let refused = reply.expect_err("a revoked owner gets no reply");
    assert!(
        refused == failure("owner_authentication_failed")
            || refused == failure("owner_grant_stale_or_revoked")
            || unanswered.contains(&refused.0.as_str()),
        "{refused:?}"
    );
    let again = tokio::time::timeout(
        Duration::from_secs(5),
        client.watch(Cursor::new(0), None, 1_000),
    )
    .await
    .expect("a revoked handshake is refused at once, not after the client's timeout")
    .expect_err("a revoked owner is not admitted again");
    assert!(unanswered.contains(&again.0.as_str()), "{again:?}");
    owner.stop().await;
}

/// The guard of rv-sec2 S3a F1: a watch parked past its first owner check when the owner is
/// revoked is answered only the refusal of its second check, never what woke it. The row
/// holds the owner barrier across the revoke, so no reply can be written before the second
/// check; a reply a background commit got out before the barrier, even one sent before the
/// watch parked, is read and the watch parks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_parked_at_a_revocation_is_answered_only_the_second_checks_refusal() {
    let owner = super::super::usage_tests::Usage::start().await;
    let runtime = owner.runtime();
    let packet = owner.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let first = owner
        .client
        .watch(Cursor::new(0), None, 1_000)
        .await
        .unwrap();
    let WatchReply::Changed {
        mut cursor,
        mut mark,
        ..
    } = first
    else {
        panic!("a first watch returns at once, got {first:?}");
    };
    let waiting = || runtime.authority.watches.waiting.load(Ordering::SeqCst);
    for _ in 0..8 {
        let body = serde_json::to_vec(&json!({
            "watch": { "after": cursor, "mark": mark, "wait_ms": 10_000 }
        }))
        .unwrap();
        let (server, client) = crate::spawn_gate::unix_pair_std().unwrap();
        for end in [&server, &client] {
            end.set_nonblocking(true).unwrap();
        }
        let mut server = UnixStream::from_std(server).unwrap();
        let mut client = UnixStream::from_std(client).unwrap();
        let (engine, authority) = (runtime.engine.clone(), runtime.authority.clone());
        let admission = admission.clone();
        let serving = tokio::spawn(async move {
            super::serve(&mut server, &engine, &authority, &packet, &admission, &body).await
        });
        let parked = Clock::now() + Duration::from_secs(5);
        while waiting() == 0 && !serving.is_finished() {
            assert!(
                Clock::now() < parked,
                "the watch parks past its first check or answers"
            );
            tokio::task::yield_now().await;
        }
        let barrier = runtime.engine.owner_egress.write().await;
        let early = tokio::time::timeout(Duration::from_millis(50), read_frame(&mut client)).await;
        if let Ok(frame) = early {
            let reply: WatchReply = serde_json::from_slice(&frame.unwrap()[4..]).unwrap();
            let (WatchReply::Changed {
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
            }) = reply
            else {
                panic!("an owner not yet revoked is answered, got {reply:?}");
            };
            (cursor, mark) = (next, moved);
            drop(barrier);
            serving.await.unwrap().unwrap();
            continue;
        }
        let control = OwnerControl {
            operation_id: RequestId::new("revoke-parked-watch").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        };
        let (revoked, registration) = runtime
            .engine
            .owner_job(|reply| Job::OwnerControl(runtime.authority.host.clone(), control, reply))
            .await
            .unwrap();
        runtime
            .authority
            .publish(registration, runtime.authority.next_control())
            .unwrap();
        assert!(!revoked.active);
        drop(barrier);
        let frame = tokio::time::timeout(Duration::from_secs(15), read_frame(&mut client))
            .await
            .expect("the parked watch is answered once the barrier drops")
            .unwrap();
        let reply: WatchReply = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(
            reply,
            WatchReply::Rejected {
                code: "owner_authentication_failed".into()
            }
        );
        serving.await.unwrap().unwrap();
        owner.stop().await;
        return;
    }
    panic!("background commits woke the watch before every revoke");
}

/// Creates one project through the engine's owner client, at the revision a capabilities
/// read gives, so the commit names the projects topic. Inputs: the engine and the project's
/// id. Output: none; it panics unless the change applied.
async fn create_project(owner: &super::super::usage_tests::Usage, id: &str) {
    let read = OwnerRequest {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        request_id: RequestId::new(format!("{id}-read")).unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read {
            query: OwnerRead::Capabilities,
        },
    };
    let OwnerResponse::Read { state } = owner.client.request(read).await.unwrap().response else {
        panic!("the capabilities read failed");
    };
    let created = owner
        .client
        .request(OwnerRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: state.revision,
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: ProjectId::new(id).unwrap(),
                    spec: ProjectSpec {
                        name: id.into(),
                        repositories: vec![],
                    },
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = &created.response else {
        panic!("not applied: {created:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied, "{created:?}");
}

/// Reads the turns a follow hands on until one names `topic`, for at most ten seconds, two
/// poll periods. Inputs: the receiver and the topic. Output: that turn's topics.
async fn handed(
    received: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<Topic>>,
    topic: Topic,
) -> Vec<Topic> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let topics = tokio::time::timeout_at(deadline, received.recv())
            .await
            .unwrap_or_else(|_| panic!("no turn named {topic:?} in time"))
            .expect("the follow still runs");
        if topics.contains(&topic) {
            return topics;
        }
    }
}

/// The bridge's follow on a live engine (v2.5 S3b): an owner change reaches its receiver
/// with its topic, and a change made while the follow has no connection arrives once it has
/// one again, from the cursor it kept, after which nothing is handed on a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_followed_watch_hands_on_each_change_and_one_made_while_it_had_no_connection() {
    let owner = super::super::usage_tests::Usage::start().await;
    let watcher = Arc::new(
        OwnerClient::attach(
            &owner.client.root.path,
            OwnerClientKind::Tui,
            Duration::from_secs(20),
        )
        .unwrap(),
    );
    let connected = Arc::new(AtomicBool::new(true));
    let (missed, mut missing) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel::<Vec<Topic>>();
    let link = Arc::clone(&connected);
    let following = tokio::spawn(crate::watch_feed::follow(
        move || {
            let client = link.load(Ordering::SeqCst).then(|| Arc::clone(&watcher));
            if client.is_none() {
                let _ = missed.send(());
            }
            std::future::ready(client)
        },
        move |topics: &[Topic]| sent.send(topics.to_vec()).is_ok(),
    ));
    let first = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .expect("a first watch has no mark, so it is answered at once")
        .unwrap();
    assert_eq!(first.last(), Some(&Topic::Unknown), "{first:?}");
    create_project(&owner, "followed-1").await;
    let applied = Clock::now();
    handed(&mut received, Topic::Projects).await;
    assert!(applied.elapsed() < WAKE, "{:?}", applied.elapsed());
    connected.store(false, Ordering::SeqCst);
    create_project(&owner, "followed-2").await;
    tokio::time::timeout(Duration::from_secs(5), missing.recv())
        .await
        .expect("a watch in flight ends with the change, and the next turn has no connection")
        .unwrap();
    create_project(&owner, "followed-3").await;
    while received.try_recv().is_ok() {}
    connected.store(true, Ordering::SeqCst);
    handed(&mut received, Topic::Projects).await;
    let deadline = tokio::time::Instant::now() + WAKE;
    while let Ok(topics) = tokio::time::timeout_at(deadline, received.recv()).await {
        let topics = topics.expect("the follow still runs");
        assert!(
            !topics.contains(&Topic::Projects),
            "handed on twice: {topics:?}"
        );
    }
    following.abort();
    owner.stop().await;
}
