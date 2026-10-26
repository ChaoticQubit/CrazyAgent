#![cfg(unix)]

use crazyagent::config::resolve_config;
use crazyagent::conversation::{ConversationContent, ConversationMutation, ConversationPage};
use crazyagent::engine::owner::*;
use crazyagent::engine::{Client, Engine, EngineError, Server};
use crazyagent::protocol::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-conv-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn append(name: &str, text: &str) -> ConversationMutation {
    ConversationMutation::AppendMessage {
        project_id: project(name),
        text: text.into(),
    }
}

fn brief(name: &str, version: u64, text: &str) -> ConversationMutation {
    ConversationMutation::ReplaceBrief {
        project_id: project(name),
        expected_version: version,
        text: text.into(),
    }
}

fn mutation(id: &str, revision: u64, change: ConversationMutation) -> OwnerRequest {
    request(
        id,
        revision,
        OwnerOperation::Apply {
            mutation: OwnerMutation::Conversation { mutation: change },
        },
    )
}

struct Service {
    engine: Arc<Engine>,
    ordinary: Client,
    control: OwnerRuntimeHandle,
    owner_path: PathBuf,
    stops: Vec<oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<Result<(), EngineError>>>,
}

impl Service {
    async fn start(root: &Path) -> Self {
        let owner_path = root.join("owner");
        if !owner_path.exists() {
            std::fs::create_dir(&owner_path).unwrap();
            std::fs::set_permissions(&owner_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = resolve_config(&root.join("global.toml"), &root.join("project.toml")).unwrap();
        let engine = Arc::new(
            Engine::open(&root.join("db"), config.clone())
                .await
                .unwrap(),
        );
        let ordinary_path = root.join("engine.sock");
        let ordinary_server = Server::bind(&ordinary_path, engine.clone(), &config).unwrap();
        let (owner_server, control) = OwnerServer::bind(&owner_path, engine.clone(), &config)
            .await
            .unwrap();
        let (ordinary_stop, ordinary_recv) = oneshot::channel();
        let (owner_stop, owner_recv) = oneshot::channel();
        let tasks = vec![
            tokio::spawn(ordinary_server.run(ordinary_recv)),
            tokio::spawn(owner_server.run(owner_recv)),
        ];
        let ordinary = Client::new(ordinary_path, Duration::from_secs(2)).unwrap();
        let result = Self {
            engine,
            ordinary,
            control,
            owner_path,
            stops: vec![owner_stop, ordinary_stop],
            tasks,
        };
        for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
            assert!(matches!(
                result
                    .read(&result.owner(kind), OwnerRead::Capabilities)
                    .await
                    .data,
                OwnerData::Capabilities {
                    authenticated_owner_transport: true,
                    native_worker_admission: false,
                    ..
                }
            ));
        }
        result
    }

    fn owner(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::attach(&self.owner_path, kind, Duration::from_secs(2)).unwrap()
    }

    async fn read(&self, client: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
        let reply = client
            .request(request("read", 0, OwnerOperation::Read { query }))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read failed: {reply:?}");
        };
        state
    }

    async fn revision(&self) -> u64 {
        self.read(
            &self.owner(OwnerClientKind::Desktop),
            OwnerRead::Capabilities,
        )
        .await
        .revision
        .value()
    }

    async fn create(&self, name: &str) {
        let reply = self
            .ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("project-fixture").unwrap(),
                request_id: RequestId::new(name).unwrap(),
                expected_revision: Cursor::new(self.revision().await),
                command: Command::CreateProject {
                    project_id: project(name),
                    spec: ProjectSpec {
                        name: name.into(),
                        repositories: vec![],
                    },
                },
            })
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
    }

    async fn apply(
        &self,
        client: &OwnerClient,
        id: &str,
        change: ConversationMutation,
    ) -> OwnerApplied {
        let request = mutation(id, self.revision().await, change);
        let reply = client.request(request).await.unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!("actual owner apply failed: {reply:?}");
        };
        operation
    }

    async fn history(&self, client: &OwnerClient, name: &str) -> ConversationPage {
        let state = self
            .read(
                client,
                OwnerRead::ConversationHistory {
                    project_id: project(name),
                    cursor: None,
                    limit: 32,
                },
            )
            .await;
        let OwnerData::ConversationHistory { page } = state.data else {
            panic!("missing real history");
        };
        page
    }

    async fn stop(mut self) {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        for task in self.tasks.drain(..) {
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        self.engine.close().await.unwrap();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
    }
}

#[test]
fn actual_owner_boundary_validates_text_and_brief_version() {
    for text in ["".into(), " \n\t".into(), "a\0b".into(), "é".repeat(4097)] {
        let error = mutation("invalid", 0, append("p", &text))
            .validate()
            .unwrap_err();
        assert_eq!(error.0, "conversation_text_bounds");
    }
    mutation("valid", 0, append("p", &"é".repeat(4096)))
        .validate()
        .unwrap();
    let error = mutation("overflow", 0, brief("p", i64::MAX as u64, "brief"))
        .validate()
        .unwrap_err();
    assert_eq!(error.0, "conversation_version_bounds");
}

#[tokio::test]
async fn real_socket_conversation_and_explicit_brief_survive_replay_and_restart() {
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    let writer = service.owner(OwnerClientKind::Tui);
    let reader = service.owner(OwnerClientKind::Desktop);
    let exact = mutation(
        "message-1",
        service.revision().await,
        append("p", "unique owner message 🦀"),
    );
    let reply = writer.request(exact.clone()).await.unwrap();
    let OwnerResponse::Applied { operation } = reply.response else {
        panic!("message not applied: {reply:?}");
    };
    assert_eq!(
        operation.result,
        OwnerResult::Applied,
        "authenticated real message must persist"
    );
    let OwnerData::CurrentBrief { brief: current, .. } = service
        .read(
            &reader,
            OwnerRead::CurrentBrief {
                project_id: project("p"),
            },
        )
        .await
        .data
    else {
        panic!("missing brief projection");
    };
    assert!(
        current.is_none(),
        "ordinary message must not become a brief"
    );
    let first = service
        .apply(
            &writer,
            "brief-1",
            brief("p", 0, "explicit brief version one"),
        )
        .await;
    assert_eq!(first.result, OwnerResult::Applied);
    let second = service
        .apply(
            &reader,
            "brief-2",
            brief("p", 1, "explicit brief version two"),
        )
        .await;
    assert_eq!(second.result, OwnerResult::Applied);
    let page = service.history(&reader, "p").await;
    assert_eq!(page.entries.len(), 3);
    assert!(page.next.is_none());
    assert_eq!(page.entries[0].text, "unique owner message 🦀");
    assert_eq!(page.entries[0].content, ConversationContent::Message);
    assert_eq!(page.entries[0].provenance, operation.provenance);
    assert_eq!(
        page.entries[1].content,
        ConversationContent::Brief { version: 1 }
    );
    assert_eq!(
        page.entries[2].content,
        ConversationContent::Brief { version: 2 }
    );
    let replay = writer.request(exact.clone()).await.unwrap();
    let OwnerResponse::Applied { operation: replay } = replay.response else {
        panic!("replay unavailable");
    };
    assert_eq!(replay.result, operation.result);
    assert_eq!(replay.provenance, operation.provenance);
    assert!(replay.historical_replay);
    assert_eq!(replay.current_revision, second.current_revision);
    assert_eq!(service.history(&reader, "p").await, page);
    let OwnerData::ConversationHistory {
        page: before_restart,
    } = service
        .read(
            &reader,
            OwnerRead::ConversationHistory {
                project_id: project("p"),
                cursor: None,
                limit: 1,
            },
        )
        .await
        .data
    else {
        panic!("history cursor missing");
    };
    let old_cursor = before_restart.next.unwrap();
    drop(writer);
    drop(reader);
    service.stop().await;
    let service = Service::start(root.path()).await;
    let writer = service.owner(OwnerClientKind::Tui);
    let reader = service.owner(OwnerClientKind::Desktop);
    let after = service.history(&reader, "p").await;
    assert_eq!(after.entries, page.entries);
    assert_eq!(
        service
            .read(
                &reader,
                OwnerRead::ConversationHistory {
                    project_id: project("p"),
                    cursor: Some(old_cursor),
                    limit: 1
                }
            )
            .await
            .data,
        OwnerData::ResyncRequired
    );
    let OwnerData::CurrentBrief {
        brief: Some(current),
        ..
    } = service
        .read(
            &reader,
            OwnerRead::CurrentBrief {
                project_id: project("p"),
            },
        )
        .await
        .data
    else {
        panic!("brief lost after restart");
    };
    assert_eq!(current.text, "explicit brief version two");
    assert_eq!(current.content, ConversationContent::Brief { version: 2 });
    let OwnerResponse::Applied { operation: replay } =
        writer.request(exact).await.unwrap().response
    else {
        panic!("restart replay missing");
    };
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, operation.provenance);
    assert_eq!(replay.result, operation.result);
    assert_ne!(
        service
            .read(&writer, OwnerRead::Capabilities)
            .await
            .revision
            .value(),
        0
    );
    service.stop().await;
}

#[test]
fn owner_history_boundary_rejects_invalid_page_and_cursor_shapes() {
    use crazyagent::conversation::ConversationCursor;
    let cursor = ConversationCursor {
        project_id: project("p"),
        db_instance: "a".repeat(64),
        owner_id: "b".repeat(64),
        engine_epoch: "c".repeat(64),
        owner_epoch: 1,
        config_id: ConfigId::new("d".repeat(64)).unwrap(),
        at_revision: Cursor::new(9),
        after: Cursor::new(1),
    };
    for limit in [0, 33, u8::MAX] {
        let req = request(
            "page",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ConversationHistory {
                    project_id: project("p"),
                    cursor: None,
                    limit,
                },
            },
        );
        assert_eq!(req.validate().unwrap_err().0, "conversation_page_limit");
    }
    for (field, code) in [
        ("owner", "conversation_cursor"),
        ("after", "conversation_cursor"),
        ("epoch", "conversation_cursor"),
        ("project", "conversation_cursor_project"),
    ] {
        let mut changed = cursor.clone();
        match field {
            "owner" => changed.owner_id = "not-hex".into(),
            "after" => changed.after = Cursor::new(10),
            "epoch" => changed.owner_epoch = 0,
            "project" => changed.project_id = project("q"),
            _ => unreachable!(),
        }
        let req = request(
            "cursor",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ConversationHistory {
                    project_id: project("p"),
                    cursor: Some(changed),
                    limit: 1,
                },
            },
        );
        assert_eq!(req.validate().unwrap_err().0, code);
    }
}

fn counts(root: &Path) -> (i64, i64, i64, i64) {
    let db = rusqlite::Connection::open_with_flags(
        root.join("db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count = |table: &str| {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (
        count("conversation_entries"),
        count("events"),
        count("owner_operations"),
        db.query_row("SELECT revision FROM state WHERE id=1", [], |r| r.get(0))
            .unwrap(),
    )
}

fn schema(root: &Path) -> std::collections::BTreeMap<String, String> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn real_actor_rejects_stale_brief_revision_changed_payload_and_unknown_projects() {
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    service.create("q").await;
    let writer = service.owner(OwnerClientKind::Tui);
    assert_eq!(
        service
            .apply(
                &writer,
                "initial-brief",
                brief("p", 0, "accepted explicit brief")
            )
            .await
            .result,
        OwnerResult::Applied
    );
    let before = counts(root.path());
    let rejected = service
        .apply(&writer, "stale-brief", brief("p", 0, "must not replace"))
        .await;
    assert_eq!(
        rejected.result,
        OwnerResult::Rejected {
            code: "stale_brief_version".into()
        }
    );
    assert!(rejected.provenance.event_cursor.is_none());
    assert_eq!(
        service
            .apply(
                &writer,
                "missing-project",
                append("absent", "must not create project")
            )
            .await
            .result,
        OwnerResult::Rejected {
            code: "project_missing".into()
        }
    );
    let stale = writer
        .request(mutation("stale-authority", 0, append("p", "not current")))
        .await
        .unwrap();
    assert!(
        matches!(stale.response, OwnerResponse::Applied { operation: OwnerApplied { result: OwnerResult::Rejected { ref code }, .. } } if code == "stale_revision")
    );
    let exact = mutation(
        "exact-identity",
        service.revision().await,
        append("p", "original immutable message"),
    );
    assert!(matches!(
        writer.request(exact.clone()).await.unwrap().response,
        OwnerResponse::Applied {
            operation: OwnerApplied {
                result: OwnerResult::Applied,
                ..
            }
        }
    ));
    let changed = mutation(
        "exact-identity",
        exact.expected_revision.value(),
        append("p", "changed payload must fail"),
    );
    assert_eq!(
        writer.request(changed).await.unwrap().response,
        OwnerResponse::Rejected {
            code: "operation_identity_conflict".into()
        }
    );
    let after = counts(root.path());
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(after.1, before.1 + 1);
    assert_eq!(after.3, before.3 + 1);
    assert_eq!(service.history(&writer, "q").await.entries.len(), 0);
    let OwnerData::CurrentBrief {
        brief: None,
        project_id,
    } = service
        .read(
            &writer,
            OwnerRead::CurrentBrief {
                project_id: project("q"),
            },
        )
        .await
        .data
    else {
        panic!("brief crossed project boundary");
    };
    assert_eq!(project_id, project("q"));
    let missing = writer
        .request(request(
            "missing-read",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ConversationHistory {
                    project_id: project("absent"),
                    cursor: None,
                    limit: 1,
                },
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        missing.response,
        OwnerResponse::Rejected {
            code: "project_missing".into()
        }
    );
    service.stop().await;
}

#[tokio::test]
async fn actual_owner_cursors_are_project_and_snapshot_bound_and_revocation_gates_replay() {
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    service.create("q").await;
    let writer = service.owner(OwnerClientKind::Tui);
    let reader = service.owner(OwnerClientKind::Desktop);
    let exact = mutation(
        "saved-message",
        service.revision().await,
        append("p", "first"),
    );
    let OwnerResponse::Applied { operation } =
        writer.request(exact.clone()).await.unwrap().response
    else {
        panic!("write failed");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_eq!(
        service
            .apply(&writer, "next-message", append("p", "second"))
            .await
            .result,
        OwnerResult::Applied
    );
    let OwnerData::ConversationHistory { page } = service
        .read(
            &reader,
            OwnerRead::ConversationHistory {
                project_id: project("p"),
                cursor: None,
                limit: 1,
            },
        )
        .await
        .data
    else {
        panic!("history missing");
    };
    let cursor = page.next.unwrap();
    let cross = request(
        "cross-project",
        0,
        OwnerOperation::Read {
            query: OwnerRead::ConversationHistory {
                project_id: project("q"),
                cursor: Some(cursor.clone()),
                limit: 1,
            },
        },
    );
    assert_eq!(
        cross.validate().unwrap_err().0,
        "conversation_cursor_project"
    );
    let wrong_owner = service
        .read(
            &writer,
            OwnerRead::ConversationHistory {
                project_id: project("p"),
                cursor: Some(cursor.clone()),
                limit: 1,
            },
        )
        .await;
    assert_eq!(wrong_owner.data, OwnerData::ResyncRequired);
    assert_eq!(
        service
            .apply(&writer, "other-project", append("q", "q only"))
            .await
            .result,
        OwnerResult::Applied
    );
    let stale = service
        .read(
            &reader,
            OwnerRead::ConversationHistory {
                project_id: project("p"),
                cursor: Some(cursor.clone()),
                limit: 1,
            },
        )
        .await;
    assert_eq!(stale.data, OwnerData::ResyncRequired);
    let mut forged = cursor;
    forged.at_revision = Cursor::new(service.revision().await);
    forged.after = forged.at_revision.clone();
    let missing_entry = reader
        .request(request(
            "foreign-entry",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ConversationHistory {
                    project_id: project("p"),
                    cursor: Some(forged),
                    limit: 1,
                },
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        missing_entry.response,
        OwnerResponse::Rejected {
            code: "conversation_cursor".into()
        }
    );
    let before = counts(root.path());
    service
        .control
        .control(OwnerControl {
            operation_id: RequestId::new("revoke-writer").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: service.control.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    let replay = writer.request(exact).await;
    assert_eq!(
        replay.unwrap_err().0,
        "owner_outcome_unknown_reconcile_same_identity"
    );
    let blocked = writer
        .request(request(
            "revoked-read",
            0,
            OwnerOperation::Read {
                query: OwnerRead::CurrentBrief {
                    project_id: project("p"),
                },
            },
        ))
        .await;
    assert_eq!(
        blocked.unwrap_err().0,
        "owner_outcome_unknown_reconcile_same_identity"
    );
    assert_eq!(
        writer
            .request(mutation(
                "revoked-new-write",
                service.revision().await,
                append("p", "must remain denied")
            ))
            .await
            .unwrap_err()
            .0,
        "owner_outcome_unknown_reconcile_same_identity"
    );
    let after = counts(root.path());
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    assert_eq!(after.2, before.2 + 1);
    assert_eq!(after.3, before.3);
    assert_eq!(service.history(&reader, "p").await.entries.len(), 2);
    service.stop().await;
}

#[tokio::test]
async fn worst_case_text_pages_progress_without_loss_and_brief_remains_explicit() {
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    let writer = service.owner(OwnerClientKind::Tui);
    let reader = service.owner(OwnerClientKind::Desktop);
    let text = "\u{1b}".repeat(crazyagent::conversation::TEXT_BYTES_MAX);
    for n in 0..4 {
        assert_eq!(
            service
                .apply(&writer, &format!("escaped-{n}"), append("p", &text))
                .await
                .result,
            OwnerResult::Applied
        );
    }
    let mut cursor = None;
    let mut all = vec![];
    let mut pages = 0;
    loop {
        let reply = reader
            .request(request(
                "page",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::ConversationHistory {
                        project_id: project("p"),
                        cursor,
                        limit: 32,
                    },
                },
            ))
            .await
            .unwrap();
        assert!(encode_frame(&reply).unwrap().len() <= FRAME_BYTES_MAX + 4);
        let OwnerResponse::Read {
            state:
                OwnerReadReply {
                    data: OwnerData::ConversationHistory { page },
                    ..
                },
        } = reply.response
        else {
            panic!("actual large-text page unavailable");
        };
        assert!(!page.entries.is_empty());
        pages += 1;
        all.extend(page.entries);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 5);
    }
    assert_eq!(pages, 4);
    assert_eq!(all.len(), 4);
    assert!(
        all.windows(2)
            .all(|w| w[0].event_cursor.value() < w[1].event_cursor.value())
    );
    assert!(
        all.iter()
            .all(|e| e.text == text && e.content == ConversationContent::Message)
    );
    let OwnerData::CurrentBrief { brief: None, .. } = service
        .read(
            &reader,
            OwnerRead::CurrentBrief {
                project_id: project("p"),
            },
        )
        .await
        .data
    else {
        panic!("message implicitly created a brief");
    };
    service.stop().await;
}

#[tokio::test]
async fn ordinary_wire_and_typed_legacy_call_cannot_mutate_owner_conversation() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    let command = Command::OwnerConversation {
        mutation: append("p", "ordinary command must fail"),
    };
    let wire = WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("ordinary").unwrap(),
        request_id: RequestId::new("ordinary-conversation").unwrap(),
        expected_revision: Cursor::new(service.revision().await),
        command,
    };
    let before = counts(root.path());
    let body = serde_json::to_value(&wire).unwrap();
    assert!(serde_json::from_value::<WireRequest>(body).is_err());
    let mut socket = crazyagent::spawn_gate::connect_unix(root.path().join("engine.sock"))
        .await
        .unwrap();
    socket
        .write_all(&encode_frame(&wire).unwrap())
        .await
        .unwrap();
    let mut byte = [0; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(
        service.engine.apply(wire).await.unwrap_err().0,
        "owner_authentication_required"
    );
    assert_eq!(counts(root.path()), before);
    service.stop().await;
}

#[tokio::test]
async fn discarded_real_socket_ack_reconciles_one_atomic_effect_and_event() {
    use tokio::io::AsyncWriteExt;
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    let reader = service.owner(OwnerClientKind::Desktop);
    let writer = service.owner(OwnerClientKind::Tui);
    let exact = mutation(
        "unobserved-ack",
        service.revision().await,
        append("p", "real transport message with discarded acknowledgement"),
    );
    let before = counts(root.path());
    let key = std::fs::read(service.owner_path.join("owner-tui.cap")).unwrap();
    assert_eq!(key.len(), 32);
    let mut auth = [0; 37];
    auth[..4].copy_from_slice(b"CAO1");
    auth[4] = 0;
    auth[5..].copy_from_slice(&key);
    let mut stream = crazyagent::spawn_gate::connect_unix(service.owner_path.join("owner.sock"))
        .await
        .unwrap();
    stream.write_all(&auth).await.unwrap();
    stream
        .write_all(&encode_frame(&exact).unwrap())
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    drop(stream);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if service.history(&reader, "p").await.entries.len() == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "real committed message not independently observed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let reconcile = request(
        "reconcile",
        0,
        OwnerOperation::Reconcile {
            operation_id: exact.request_id.clone(),
            expected_hash: Some(exact.mutation_hash().unwrap()),
        },
    );
    let OwnerResponse::Committed { operation } = writer.request(reconcile).await.unwrap().response
    else {
        panic!("actual operation not committed");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(operation.historical_replay);
    assert_eq!(
        operation.provenance.event_cursor.as_ref(),
        Some(&operation.provenance.revision_after)
    );
    let after = counts(root.path());
    assert_eq!(
        after,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    let OwnerResponse::Applied { operation: replay } =
        writer.request(exact).await.unwrap().response
    else {
        panic!("exact replay missing");
    };
    assert_eq!(replay.provenance, operation.provenance);
    assert!(replay.historical_replay);
    assert_eq!(counts(root.path()), after);
    service.stop().await;
}

#[tokio::test]
async fn actual_legacy_database_migrates_additively_and_readonly_does_not_install_schema() {
    let root = private_root();
    let config = resolve_config(
        &root.path().join("global.toml"),
        &root.path().join("project.toml"),
    )
    .unwrap();
    let engine = Engine::open(&root.path().join("db"), config.clone())
        .await
        .unwrap();
    let created = engine
        .apply(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("legacy").unwrap(),
            request_id: RequestId::new("legacy-project").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: project("legacy"),
                spec: ProjectSpec {
                    name: "preserved legacy row".into(),
                    repositories: vec![],
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(created.outcome, Outcome::Applied);
    engine.close().await.unwrap();
    let before = schema(root.path());
    assert!(!before.contains_key("conversation_meta"));
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    db.pragma_update(None, "user_version", 0).unwrap();
    let legacy: Vec<u8> = db
        .query_row(
            "SELECT reply FROM operations WHERE client='legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    let readonly = match Engine::open_readonly(&root.path().join("db"), config.clone()).await {
        Ok(_) => panic!("a read-only open must not upgrade a legacy database"),
        Err(error) => error.0,
    };
    assert!(
        readonly.starts_with("storage_upgrade_required:"),
        "{readonly}"
    );
    assert_eq!(schema(root.path()), before);
    let service = Service::start(root.path()).await;
    let after = schema(root.path());
    assert!(
        after.contains_key("conversation_meta"),
        "authenticated startup must install the additive conversation schema"
    );
    assert!(after.contains_key("conversation_entries"));
    for (name, sql) in &before {
        assert_eq!(after.get(name), Some(sql));
    }
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap()
            >= 2,
        "the writable start records the upgraded version"
    );
    assert_eq!(
        db.query_row(
            "SELECT reply FROM operations WHERE client='legacy'",
            [],
            |r| r.get::<_, Vec<u8>>(0)
        )
        .unwrap(),
        legacy
    );
    drop(db);
    let owner = service.owner(OwnerClientKind::Desktop);
    assert_eq!(
        service
            .apply(
                &owner,
                "after-migration",
                append("legacy", "first actual message after migration")
            )
            .await
            .result,
        OwnerResult::Applied
    );
    service.stop().await;
}

#[tokio::test]
async fn unknown_partial_conversation_schema_is_preserved_and_owner_start_rolls_back() {
    let root = private_root();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Engine::open(&root.path().join("db"), config.clone())
        .await
        .unwrap();
    engine.close().await.unwrap();
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    db.execute_batch("CREATE TABLE conversation_meta (unknown TEXT); INSERT INTO conversation_meta VALUES ('preserve-me');").unwrap();
    drop(db);
    let before = schema(root.path());
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    let owner_path = root.path().join("owner");
    std::fs::create_dir(&owner_path).unwrap();
    std::fs::set_permissions(&owner_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = OwnerServer::bind(&owner_path, engine.clone(), &config).await;
    assert!(
        result.is_err(),
        "unknown conversation schema must refuse owner startup"
    );
    assert_eq!(result.err().unwrap().0, "conversation_schema_unknown");
    assert_eq!(schema(root.path()), before);
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT unknown FROM conversation_meta", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "preserve-me"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn full_project_history_rejects_new_effects_and_preserves_exact_old_replay() {
    let root = private_root();
    let service = Service::start(root.path()).await;
    service.create("p").await;
    let writer = service.owner(OwnerClientKind::Tui);
    let exact = mutation(
        "capacity-first",
        service.revision().await,
        append("p", "retained first entry"),
    );
    let OwnerResponse::Applied { operation } =
        writer.request(exact.clone()).await.unwrap().response
    else {
        panic!("first write missing");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    for n in 1..crazyagent::conversation::ENTRIES_MAX {
        assert_eq!(
            service
                .apply(
                    &writer,
                    &format!("capacity-{n}"),
                    append("p", "bounded history")
                )
                .await
                .result,
            OwnerResult::Applied
        );
    }
    let before = counts(root.path());
    let rejected = service
        .apply(
            &writer,
            "capacity-rejected",
            append("p", "must not evict history"),
        )
        .await;
    assert_eq!(
        rejected.result,
        OwnerResult::Rejected {
            code: "conversation_capacity".into()
        }
    );
    assert_eq!(
        counts(root.path()),
        (before.0, before.1, before.2 + 1, before.3)
    );
    let before_replay = counts(root.path());
    let OwnerResponse::Applied { operation: replay } =
        writer.request(exact).await.unwrap().response
    else {
        panic!("old replay must remain available");
    };
    assert_eq!(replay.result, operation.result);
    assert_eq!(replay.provenance, operation.provenance);
    assert!(replay.historical_replay);
    assert_eq!(counts(root.path()), before_replay);
    service.stop().await;
}
