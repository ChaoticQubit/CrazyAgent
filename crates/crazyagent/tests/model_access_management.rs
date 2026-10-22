use crazyagent::config::{Origin, resolve_config};
use crazyagent::engine::Engine;
use crazyagent::engine::owner::*;
use crazyagent::protocol::{Cursor, PROTOCOL_VERSION, RequestId};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

#[test]
fn model_access_edit_has_an_owner_wire_contract() {
    let wire = serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "metadata-edit",
        "expected_revision": "0",
        "operation": {"kind": "Apply", "mutation": {
            "kind": "ModelAccess",
            "expected_config_id": "config",
            "expected_source_hash": "a".repeat(64),
            "edit": {"kind": "add_account", "account_ref": "account-one"}
        }}
    });
    let request: OwnerRequest =
        serde_json::from_value(wire).expect("typed model access owner edit");
    request.validate().unwrap();
    assert_eq!(request.mutation_hash().unwrap().len(), 64);
}

#[tokio::test]
async fn explicit_host_sources_publish_one_durable_edit() {
    let root = tempfile::Builder::new()
        .prefix("ca-ma259-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 1000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = resolve_config(&global, &project).unwrap();
    let engine = Arc::new(
        Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
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
    let (state, source_hash, _) =
        catalog_page(&client, ModelAccessCollection::Accounts, None, 16).await;
    let edit = owner_request(
        "add-account",
        0,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ModelAccess {
                expected_config_id: state.config_id.clone(),
                expected_source_hash: source_hash,
                edit: crazyagent::config::ModelAccessEdit::AddAccount {
                    account_ref: crazyagent::protocol::CredentialRef::new("account-one").unwrap(),
                },
            },
        },
    );
    let result = client.request(edit).await;
    let _ = stop.send(());
    task.await.unwrap().unwrap();
    drop(host);
    engine.close().await.unwrap();
    let OwnerResponse::Applied { operation } = result.unwrap().response else {
        panic!("actual owner apply required")
    };
    assert_eq!(
        operation.result,
        OwnerResult::Applied,
        "explicit host sources must persist the admitted metadata edit"
    );
    assert_eq!(operation.current_revision.value(), 1);
    assert!(
        std::fs::read_to_string(&global)
            .unwrap()
            .contains("account-one")
    );
    assert_eq!(
        resolve_config(&global, &project)
            .unwrap()
            .settings
            .model_access
            .unwrap()
            .accounts
            .len(),
        1
    );
}

async fn read(client: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
    let reply = client
        .request(owner_request("read", 0, OwnerOperation::Read { query }))
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("authoritative owner read required: {:?}", reply.response)
    };
    state
}

async fn catalog_page(
    client: &OwnerClient,
    collection: ModelAccessCollection,
    cursor: Option<ModelAccessCursor>,
    limit: u8,
) -> (OwnerReadReply, String, Vec<ModelAccessRecord>) {
    let state = read(
        client,
        OwnerRead::ModelAccessPage {
            collection,
            cursor,
            limit,
        },
    )
    .await;
    let OwnerData::ModelAccessPage {
        source_hash, items, ..
    } = &state.data
    else {
        panic!("catalog page required")
    };
    (state.clone(), source_hash.clone(), items.clone())
}

struct Fixture {
    root: tempfile::TempDir,
    engine: Arc<Engine>,
    client: OwnerClient,
    host: OwnerRuntimeHandle,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), crazyagent::engine::EngineError>>,
}

impl Fixture {
    async fn start(writable: bool, global_bytes: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-m259-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, global_bytes).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let project = root.path().join("project.toml");
        std::fs::write(&project, "[routing]\npriority_weight = 3\n").unwrap();
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = resolve_config(&global, &project).unwrap();
        let db = root.path().join("engine.db");
        let engine = Arc::new(if writable {
            Engine::open_with_settings_sources(&db, &global, &project)
                .await
                .unwrap()
        } else {
            Engine::open(&db, config.clone()).await.unwrap()
        });
        Self::bind(root, engine, &config).await
    }

    async fn bind(
        root: tempfile::TempDir,
        engine: Arc<Engine>,
        config: &crazyagent::config::EffectiveConfig,
    ) -> Self {
        let (server, host) = OwnerServer::bind(root.path(), engine.clone(), config)
            .await
            .unwrap();
        let (stop, rx) = oneshot::channel();
        let task = tokio::spawn(server.run(rx));
        let client =
            OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
        Self {
            root,
            engine,
            client,
            host,
            stop,
            task,
        }
    }

    async fn close(self) -> tempfile::TempDir {
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
        drop(self.host);
        self.engine.close().await.unwrap();
        self.root
    }

    async fn request(&self, id: &str, edit: crazyagent::config::ModelAccessEdit) -> OwnerRequest {
        let (state, source_hash, _) =
            catalog_page(&self.client, ModelAccessCollection::Accounts, None, 16).await;
        owner_request(
            id,
            state.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                    edit,
                },
            },
        )
    }

    async fn edit(&self, id: &str, edit: crazyagent::config::ModelAccessEdit) -> OwnerApplied {
        let request = self.request(id, edit).await;
        let response = self.client.request(request).await.unwrap().response;
        let OwnerResponse::Applied { operation } = response else {
            panic!("actual mutation result required: {response:?}")
        };
        operation
    }
}

fn reference(value: &str) -> crazyagent::protocol::CredentialRef {
    crazyagent::protocol::CredentialRef::new(value).unwrap()
}

fn connection() -> crazyagent::config::ModelConnection {
    crazyagent::config::ModelConnection {
        id: reference("connection"),
        provider: reference("self-hosted"),
        harness: reference("gateway"),
        protocol: reference("openai-compatible"),
        account_ref: reference("account"),
        endpoint: crazyagent::config::ModelEndpoint::Http {
            url: "http://127.0.0.1:1234/v1".into(),
        },
        model: crazyagent::config::ModelReference {
            id: "model".into(),
            author: reference("model-author"),
        },
        auth: crazyagent::config::ModelAuthentication::None {},
    }
}

#[tokio::test]
async fn eight_edits_preserve_unrelated_toml_and_project_precedence() {
    use crazyagent::config::ModelAccessEdit::*;
    let fixture = Fixture::start(true, "version  = 1\n\n[routing]\npriority_weight = 2\nglobal_capacity = { slots = 32, cpu_millis = 16000, memory_mib = 8192, processes = 128, output_bytes = 67108864, wall_time_ms = 86400000, spend_microunits = 0 }\n[knowledge]\ntext_bytes_max = 2048\n").await;
    let path = fixture.root.path().join("global.toml");
    let project = std::fs::read(fixture.root.path().join("project.toml")).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixture
            .edit(
                "add",
                AddAccount {
                    account_ref: reference("account")
                }
            )
            .await
            .result,
        OwnerResult::Applied
    );
    let account_only_id = resolve_config(&path, &fixture.root.path().join("project.toml"))
        .unwrap()
        .id;
    assert_eq!(
        fixture
            .edit(
                "create-connection",
                CreateConnection {
                    connection: connection()
                }
            )
            .await
            .current_revision
            .value(),
        2
    );
    let pool = crazyagent::config::AccountPool {
        id: reference("pool"),
        members: vec![reference("connection")],
        projects: vec![reference("project")],
        roles: vec![reference("implementer")],
        fallback_allowed: true,
        provider_quota_policy: crazyagent::config::ProviderQuotaPolicy::BoundedAttempt,
    };
    assert_eq!(
        fixture
            .edit("create-pool", CreatePool { pool: pool.clone() })
            .await
            .current_revision
            .value(),
        3
    );
    let denied = fixture
        .edit(
            "referenced-connection",
            RemoveConnection {
                id: reference("connection"),
            },
        )
        .await;
    assert!(matches!(denied.result, OwnerResult::Rejected { .. }));
    assert_eq!(denied.current_revision.value(), 3);
    assert!(matches!(
        fixture
            .edit(
                "referenced-account",
                RemoveAccount {
                    account_ref: reference("account")
                }
            )
            .await
            .result,
        OwnerResult::Rejected { .. }
    ));
    let mut replacement = connection();
    replacement.model.id = "next-model".into();
    assert_eq!(
        fixture
            .edit(
                "replace-connection",
                ReplaceConnection {
                    connection: replacement.clone()
                }
            )
            .await
            .current_revision
            .value(),
        4
    );
    let record = read(
        &fixture.client,
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: reference("connection"),
        },
    )
    .await;
    assert!(
        matches!(record.data, OwnerData::ModelAccessRecord { record: ModelAccessRecord::Connection { connection }, .. } if connection == replacement)
    );
    let mut replacement_pool = pool;
    replacement_pool.fallback_allowed = false;
    assert_eq!(
        fixture
            .edit(
                "replace-pool",
                ReplacePool {
                    pool: replacement_pool
                }
            )
            .await
            .current_revision
            .value(),
        5
    );
    assert_eq!(
        fixture
            .edit(
                "remove-pool",
                RemovePool {
                    id: reference("pool")
                }
            )
            .await
            .current_revision
            .value(),
        6
    );
    assert_eq!(
        fixture
            .edit(
                "remove-connection",
                RemoveConnection {
                    id: reference("connection")
                }
            )
            .await
            .current_revision
            .value(),
        7
    );
    assert_eq!(
        resolve_config(&path, &fixture.root.path().join("project.toml"))
            .unwrap()
            .id,
        account_only_id
    );
    assert_eq!(
        fixture
            .edit(
                "remove-account",
                RemoveAccount {
                    account_ref: reference("account")
                }
            )
            .await
            .current_revision
            .value(),
        8
    );
    let current = std::fs::read_to_string(&path).unwrap();
    assert!(
        current.starts_with(&original),
        "unrelated TOML text must survive exactly"
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("project.toml")).unwrap(),
        project
    );
    let effective = resolve_config(&path, &fixture.root.path().join("project.toml")).unwrap();
    assert_eq!(effective.settings.routing.priority_weight, 3);
    assert_eq!(effective.settings.routing.global_capacity.slots, 32);
    assert_eq!(effective.settings.knowledge.text_bytes_max, 2048);
    assert_eq!(effective.descriptors.len(), 26);
    assert!(effective.settings.model_access.unwrap().accounts.is_empty());
    let root = fixture.close().await;
    let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM settings_publication", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM configs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
}

#[tokio::test]
async fn replay_conflicts_and_cursor_revision_are_authoritative() {
    use crazyagent::config::ModelAccessEdit::AddAccount;
    let f = Fixture::start(true, "").await;
    let original = f
        .request(
            "one",
            AddAccount {
                account_ref: reference("one"),
            },
        )
        .await;
    let stale = f
        .request(
            "stale",
            AddAccount {
                account_ref: reference("two"),
            },
        )
        .await;
    let first = f.client.request(original.clone()).await.unwrap();
    let replay = f.client.request(original.clone()).await.unwrap();
    assert!(
        matches!(first.response, OwnerResponse::Applied { ref operation } if operation.current_revision.value() == 1)
    );
    assert!(
        matches!(replay.response, OwnerResponse::Applied { operation } if operation.historical_replay && operation.current_revision.value() == 1)
    );
    assert!(
        matches!(f.client.request(stale).await.unwrap().response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Rejected { code: "stale_revision".into() })
    );
    let mut conflicting = original.clone();
    if let OwnerOperation::Apply {
        mutation: OwnerMutation::ModelAccess { edit, .. },
    } = &mut conflicting.operation
    {
        *edit = AddAccount {
            account_ref: reference("changed"),
        };
    }
    assert!(
        matches!(f.client.request(conflicting).await.unwrap().response, OwnerResponse::Rejected { code } if code == "operation_identity_conflict")
    );
    let mut stale_config = f
        .request(
            "old-config",
            AddAccount {
                account_ref: reference("two"),
            },
        )
        .await;
    let OwnerOperation::Apply {
        mutation: OwnerMutation::ModelAccess {
            expected_config_id, ..
        },
    } = &mut stale_config.operation
    else {
        panic!("model access mutation required")
    };
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ModelAccess {
                expected_config_id: old,
                ..
            },
    } = &original.operation
    else {
        panic!("original model access mutation required")
    };
    *expected_config_id = old.clone();
    assert!(
        matches!(f.client.request(stale_config).await.unwrap().response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Rejected { code: "stale_config".into() })
    );
    assert_eq!(
        f.edit(
            "two",
            AddAccount {
                account_ref: reference("two")
            }
        )
        .await
        .result,
        OwnerResult::Applied
    );
    let (state, _, items) = catalog_page(&f.client, ModelAccessCollection::Accounts, None, 1).await;
    assert_eq!(items.len(), 1);
    let OwnerData::ModelAccessPage {
        next: Some(cursor), ..
    } = state.data
    else {
        panic!("bounded cursor required")
    };
    let (_, _, items) = catalog_page(
        &f.client,
        ModelAccessCollection::Accounts,
        Some(cursor.clone()),
        1,
    )
    .await;
    assert_eq!(items[0].id(), &reference("two"));
    f.edit(
        "three",
        AddAccount {
            account_ref: reference("three"),
        },
    )
    .await;
    assert!(matches!(
        read(
            &f.client,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Accounts,
                cursor: Some(cursor),
                limit: 1
            }
        )
        .await
        .data,
        OwnerData::ResyncRequired
    ));
    f.close().await;
}

#[tokio::test]
async fn old_constructor_is_read_only_and_worker_cannot_edit() {
    let f = Fixture::start(false, "").await;
    let bytes = std::fs::read(f.root.path().join("global.toml")).unwrap();
    let request = f
        .request(
            "denied",
            crazyagent::config::ModelAccessEdit::AddAccount {
                account_ref: reference("account"),
            },
        )
        .await;
    let wire: crazyagent::protocol::WireRequest = serde_json::from_value(serde_json::json!({
        "protocol_version": 1, "client_id": "worker", "request_id": "worker-write", "expected_revision": "0", "command": {"kind": "Snapshot"}
    })).unwrap();
    let mut worker = wire;
    worker.command = crazyagent::protocol::Command::OwnerModelAccess {
        expected_config_id: resolve_config(
            &f.root.path().join("global.toml"),
            &f.root.path().join("project.toml"),
        )
        .unwrap()
        .id,
        expected_source_hash: "a".repeat(64),
        edit: crazyagent::config::ModelAccessEdit::AddAccount {
            account_ref: reference("worker"),
        },
    };
    assert_eq!(
        f.engine.apply(worker.clone()).await.unwrap_err().0,
        "owner_authentication_required"
    );
    assert!(
        serde_json::from_slice::<crazyagent::protocol::WireRequest>(
            &serde_json::to_vec(&worker).unwrap()
        )
        .is_err()
    );
    assert!(
        matches!(f.client.request(request).await.unwrap().response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Rejected { code: "settings_read_only".into() })
    );
    assert_eq!(
        std::fs::read(f.root.path().join("global.toml")).unwrap(),
        bytes
    );
    let capabilities = read(&f.client, OwnerRead::Capabilities).await;
    assert!(matches!(
        capabilities.data,
        OwnerData::Capabilities {
            native_worker_admission: false,
            settings_persistence: false,
            ..
        }
    ));
    f.close().await;
}

#[tokio::test]
async fn real_sqlite_abort_retains_unknown_and_recovers_exact_old_or_new_file() {
    for before_rename in [true, false] {
        let f = Fixture::start(true, "version = 1\n").await;
        let request = f
            .request(
                "recover",
                crazyagent::config::ModelAccessEdit::AddAccount {
                    account_ref: reference("account"),
                },
            )
            .await;
        let db = rusqlite::Connection::open(f.root.path().join("engine.db")).unwrap();
        let trigger = if before_rename {
            "CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'actual_journal_abort'); END;"
        } else {
            "CREATE TRIGGER publication_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT, 'actual_ledger_abort'); END;"
        };
        db.execute_batch(trigger).unwrap();
        assert!(matches!(
            f.client.request(request.clone()).await.unwrap().response,
            OwnerResponse::OutcomeUnknown { .. }
        ));
        assert!(matches!(
            f.client
                .request(owner_request(
                    "reconcile",
                    0,
                    OwnerOperation::Reconcile {
                        operation_id: request.request_id.clone(),
                        expected_hash: Some(request.mutation_hash().unwrap())
                    }
                ))
                .await
                .unwrap()
                .response,
            OwnerResponse::OutcomeUnknown { .. }
        ));
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM settings_publication", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let file = std::fs::read_to_string(f.root.path().join("global.toml")).unwrap();
        assert_eq!(file.contains("account"), !before_rename);
        let config = resolve_config(
            &f.root.path().join("global.toml"),
            &f.root.path().join("project.toml"),
        )
        .unwrap();
        assert_eq!(
            f.engine.update_config(config).await.unwrap_err().0,
            "settings_publication_pending"
        );
        let root = f.close().await;
        db.execute_batch("DROP TRIGGER publication_abort").unwrap();
        drop(db);
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let engine = Arc::new(
            Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
                .await
                .unwrap(),
        );
        let config = resolve_config(&global, &project).unwrap();
        let f = Fixture::bind(root, engine, &config).await;
        let replay = f.client.request(request).await.unwrap();
        assert!(
            matches!(replay.response, OwnerResponse::Applied { operation } if operation.historical_replay && operation.current_revision.value() == 1 && operation.result == OwnerResult::Applied)
        );
        let root = f.close().await;
        let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM settings_publication", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn changed_file_or_project_conflicts_without_publication() {
    for change_project in [false, true] {
        let f = Fixture::start(true, "version = 1\n").await;
        let request = f
            .request(
                "source-race",
                crazyagent::config::ModelAccessEdit::AddAccount {
                    account_ref: reference("account"),
                },
            )
            .await;
        let target = f.root.path().join(if change_project {
            "project.toml"
        } else {
            "global.toml"
        });
        let previous = std::fs::read(&target).unwrap();
        let replacement = f.root.path().join("replacement.toml");
        std::fs::write(&replacement, &previous).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&replacement, &target).unwrap();
        let response = f.client.request(request).await.unwrap().response;
        assert!(
            matches!(response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Rejected { code: "settings_source_conflict".into() } && operation.current_revision.value() == 0)
        );
        assert_eq!(std::fs::read(&target).unwrap(), previous);
        let root = f.close().await;
        let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM settings_publication", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn pending_recovery_never_overwrites_third_state_or_changed_project() {
    for change_project in [false, true] {
        let f = Fixture::start(true, "version = 1\n").await;
        let request = f
            .request(
                "blocked-recovery",
                crazyagent::config::ModelAccessEdit::AddAccount {
                    account_ref: reference("account"),
                },
            )
            .await;
        let db = rusqlite::Connection::open(f.root.path().join("engine.db")).unwrap();
        db.execute_batch("CREATE TRIGGER publication_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT, 'actual_recovery_abort'); END;").unwrap();
        assert!(matches!(
            f.client.request(request).await.unwrap().response,
            OwnerResponse::OutcomeUnknown { .. }
        ));
        let root = f.close().await;
        db.execute_batch("DROP TRIGGER publication_abort").unwrap();
        drop(db);
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let target = if change_project { &project } else { &global };
        let third = if change_project {
            "[routing]\npriority_weight = 4\n"
        } else {
            "version = 1\ntimeout_ms = 1500\n"
        };
        std::fs::write(target, third).unwrap();
        let result =
            Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
                .await;
        assert!(matches!(result, Err(ref e) if e.0 == "settings_source_conflict"));
        assert_eq!(std::fs::read_to_string(target).unwrap(), third);
        let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM settings_publication", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn writable_sources_require_private_files_no_links_and_one_writer() {
    let f = Fixture::start(true, "").await;
    let global = f.root.path().join("global.toml");
    let project = f.root.path().join("project.toml");
    let second =
        Engine::open_with_settings_sources(&f.root.path().join("second.db"), &global, &project)
            .await;
    assert!(matches!(second, Err(ref e) if e.0 == "settings_writer_busy"));
    let root = f.close().await;
    let real = root.path().join("original.toml");
    std::fs::rename(&global, &real).unwrap();
    std::os::unix::fs::symlink(&real, &global).unwrap();
    assert!(
        Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
            .await
            .is_err()
    );
    std::fs::remove_file(&global).unwrap();
    std::fs::hard_link(&real, &global).unwrap();
    assert!(
        Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
            .await
            .is_err()
    );
    std::fs::remove_file(&global).unwrap();
    std::fs::rename(&real, &global).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project).await, Err(ref e) if e.0 == "settings_metadata_invalid")
    );
}

#[tokio::test]
async fn two_owner_clients_share_metadata_but_not_page_cursor_identity() {
    let f = Fixture::start(true, "").await;
    for id in ["account-a", "account-b"] {
        assert_eq!(
            f.edit(
                id,
                crazyagent::config::ModelAccessEdit::AddAccount {
                    account_ref: reference(id)
                }
            )
            .await
            .result,
            OwnerResult::Applied
        );
    }
    let desktop = OwnerClient::attach(
        f.root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .unwrap();
    let (state, source, tui_items) =
        catalog_page(&f.client, ModelAccessCollection::Accounts, None, 1).await;
    let (desktop_state, desktop_source, desktop_items) =
        catalog_page(&desktop, ModelAccessCollection::Accounts, None, 1).await;
    assert_eq!(desktop_state.config_id, state.config_id);
    assert_eq!(desktop_source, source);
    assert_eq!(desktop_items, tui_items);
    let OwnerData::ModelAccessPage {
        next: Some(cursor), ..
    } = state.data
    else {
        panic!("cursor required")
    };
    assert!(matches!(
        read(
            &desktop,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Accounts,
                cursor: Some(cursor),
                limit: 1
            }
        )
        .await
        .data,
        OwnerData::ResyncRequired
    ));
    let capabilities = read(&desktop, OwnerRead::Capabilities).await;
    assert!(matches!(
        capabilities.data,
        OwnerData::Capabilities {
            settings_persistence: true,
            native_worker_admission: false,
            ..
        }
    ));
    f.close().await;
}

#[tokio::test]
async fn metadata_publication_does_not_migrate_existing_task_config() {
    use crazyagent::protocol::*;
    let f = Fixture::start(true, "").await;
    let (state, _, _) = catalog_page(&f.client, ModelAccessCollection::Accounts, None, 16).await;
    let initial = resolve_config(
        &f.root.path().join("global.toml"),
        &f.root.path().join("project.toml"),
    )
    .unwrap();
    let project = ProjectId::new("project").unwrap();
    let context = ExecutionContext {
        project_id: project.clone(),
        task_id: TaskId::new("task").unwrap(),
        config_id: state.config_id,
        snapshot_id: SnapshotId::new("synthetic-input-snapshot").unwrap(),
        baseline_id: BaselineId::new("unqualified-baseline-reference").unwrap(),
        allowed_destinations: vec![],
        budget_ref: reference("budget"),
    };
    for (revision, id, command) in [
        (
            0,
            "project",
            Command::CreateProject {
                project_id: project,
                spec: ProjectSpec {
                    name: "Project".into(),
                    repositories: vec![],
                },
            },
        ),
        (
            1,
            "task",
            Command::PinTask {
                context: context.clone(),
            },
        ),
    ] {
        let reply = f
            .engine
            .apply(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(id).unwrap(),
                expected_revision: Cursor::new(revision),
                command,
            })
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
    }
    assert_eq!(
        f.edit(
            "metadata",
            crazyagent::config::ModelAccessEdit::AddAccount {
                account_ref: reference("account")
            }
        )
        .await
        .current_revision
        .value(),
        3
    );
    let state = read(
        &f.client,
        OwnerRead::TaskConfig {
            task_id: context.task_id.clone(),
        },
    )
    .await;
    assert!(
        matches!(state.data, OwnerData::Domain { outcome } if *outcome == Outcome::Configuration { config: Box::new(initial) })
    );
    let state = read(
        &f.client,
        OwnerRead::TaskContext {
            task_id: context.task_id.clone(),
        },
    )
    .await;
    assert!(
        matches!(state.data, OwnerData::Domain { outcome } if *outcome == Outcome::Task { context })
    );
    f.close().await;
}

#[test]
fn wire_rejects_secret_fields_client_paths_and_oversized_metadata() {
    let request = serde_json::json!({
        "protocol_version": 1, "request_id": "invalid", "expected_revision": "0",
        "operation": { "kind": "Apply", "mutation": { "kind": "ModelAccess",
            "expected_config_id": "config", "expected_source_hash": "a".repeat(64),
            "edit": { "kind": "add_account", "account_ref": "account" } } }
    });
    for field in ["secret", "global_path", "settings"] {
        let mut invalid = request.clone();
        invalid["operation"]["mutation"][field] = serde_json::json!("untrusted-data");
        assert!(serde_json::from_value::<OwnerRequest>(invalid).is_err());
    }
    let mut invalid = request;
    invalid["operation"]["mutation"]["edit"]["secret"] =
        serde_json::json!("synthetic-not-a-real-secret");
    assert!(serde_json::from_value::<OwnerRequest>(invalid).is_err());
    let edit = crazyagent::config::ModelAccessEdit::CreatePool {
        pool: crazyagent::config::AccountPool {
            id: reference("pool"),
            members: vec![reference("member"); 10000],
            projects: vec![reference("project")],
            roles: vec![reference("role")],
            fallback_allowed: true,
            provider_quota_policy: crazyagent::config::ProviderQuotaPolicy::BoundedAttempt,
        },
    };
    assert_eq!(edit.validate().unwrap_err().0, "model_access_edit_bounds");
    for limit in [0, 17, u8::MAX] {
        assert!(
            owner_request(
                "limit",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::ModelAccessPage {
                        collection: ModelAccessCollection::Accounts,
                        cursor: None,
                        limit
                    }
                }
            )
            .validate()
            .is_err()
        );
    }
}

#[tokio::test]
async fn writable_bootstrap_is_explicit_and_uses_the_existing_owner_api() {
    let root = tempfile::Builder::new()
        .prefix("ca-b259-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
        root.path(),
        "ordinary.sock",
        &global,
        &project,
        &stopped,
    )
    .await
    .unwrap();
    let client =
        crazyagent::engine::Client::new(root.path().join("ordinary.sock"), Duration::from_secs(2))
            .unwrap();
    let (_, owner, _) = client.with_owner(OwnerClientKind::Desktop).await.unwrap();
    let (state, source_hash, items) =
        catalog_page(&owner, ModelAccessCollection::Accounts, None, 16).await;
    assert!(items.is_empty());
    let reply = owner
        .request(owner_request(
            "bootstrap-edit",
            state.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                    edit: crazyagent::config::ModelAccessEdit::AddAccount {
                        account_ref: reference("account"),
                    },
                },
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(reply.response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
    );
    service.shutdown().await.unwrap();
    assert!(
        std::fs::read_to_string(&global)
            .unwrap()
            .contains("account")
    );
    let config = resolve_config(&global, &project).unwrap();
    let mut service = crazyagent::engine::bootstrap::EngineService::start(
        root.path(),
        "ordinary.sock",
        &config,
        &stopped,
    )
    .await
    .unwrap();
    let client =
        crazyagent::engine::Client::new(root.path().join("ordinary.sock"), Duration::from_secs(2))
            .unwrap();
    let (_, owner, _) = client.with_owner(OwnerClientKind::Tui).await.unwrap();
    assert!(matches!(
        read(&owner, OwnerRead::Capabilities).await.data,
        OwnerData::Capabilities {
            settings_persistence: false,
            native_worker_admission: false,
            ..
        }
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_settings_lists_catalog_with_knowledge() {
    let root = tempfile::Builder::new()
        .prefix("ca-management-259-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    std::fs::write(
        &global,
        "[model_access]\naccounts = [\"declared-account\"]\n[knowledge]\ntext_bytes_max = 2048\n",
    )
    .unwrap();
    let config = resolve_config(&global, &project).unwrap();
    config.validate().unwrap();
    assert_eq!(config.descriptors.len(), 26);
    let engine = Arc::new(
        Engine::open(&root.path().join("engine.db"), config.clone())
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
    let names = client
        .request(owner_request(
            "settings",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Settings,
            },
        ))
        .await;
    let setting = client
        .request(owner_request(
            "catalog",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Setting {
                    name: "model_access".into(),
                },
            },
        ))
        .await;
    let subsequent = client
        .request(owner_request(
            "still-alive",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        ))
        .await;
    let _ = stop.send(());
    task.await.unwrap().unwrap();
    drop(host);
    let closed = engine.close().await;
    let OwnerResponse::Read { state } = names
        .expect("valid catalog settings read must succeed")
        .response
    else {
        panic!("settings read must return actual settings")
    };
    let OwnerData::Settings { names } = state.data else {
        panic!("settings names required")
    };
    assert_eq!(state.revision.value(), 0);
    assert_eq!(state.config_id, config.id);
    assert_eq!(names.len(), 26);
    assert!(names.iter().any(|name| name == "model_access"));
    let OwnerResponse::Read { state } = setting
        .expect("catalog descriptor must remain readable")
        .response
    else {
        panic!("catalog descriptor read required")
    };
    let OwnerData::Setting { descriptor } = state.data else {
        panic!("catalog descriptor required")
    };
    assert_eq!(descriptor.origin, Origin::Global);
    assert_eq!(state.revision.value(), 0);
    assert!(matches!(
        subsequent.unwrap().response,
        OwnerResponse::Read { .. }
    ));
    closed.unwrap();
}

fn quota_policy_catalog() -> String {
    r#"[model_access]
accounts = ["account"]
[[model_access.connections]]
id = "connection"
provider = "self-hosted"
harness = "gateway"
protocol = "openai-compatible"
account_ref = "account"
endpoint = { kind = "http", url = "http://127.0.0.1:1234/v1" }
model = { id = "model", author = "model-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "pool"
members = ["connection"]
projects = ["project"]
roles = ["worker"]
fallback_allowed = false
"#
    .into()
}

#[tokio::test]
async fn quota_policy_owner_publication_survives_service_restart_without_migrating_task() {
    use crazyagent::config::{ModelAccessEdit, ProviderQuotaPolicy};
    use crazyagent::protocol::*;
    let fixture = Fixture::start(true, &quota_policy_catalog()).await;
    let global = fixture.root.path().join("global.toml");
    let project_path = fixture.root.path().join("project.toml");
    let project_bytes = std::fs::read(&project_path).unwrap();
    let pinned = resolve_config(&global, &project_path).unwrap();
    let context = ExecutionContext {
        project_id: ProjectId::new("project").unwrap(),
        task_id: TaskId::new("quota-pinned-task").unwrap(),
        config_id: pinned.id.clone(),
        snapshot_id: SnapshotId::new("synthetic-input-snapshot").unwrap(),
        baseline_id: BaselineId::new("unqualified-baseline-reference").unwrap(),
        allowed_destinations: vec![],
        budget_ref: reference("budget"),
    };
    for (revision, id, command) in [
        (
            0,
            "quota-project",
            Command::CreateProject {
                project_id: context.project_id.clone(),
                spec: ProjectSpec {
                    name: "Project".into(),
                    repositories: vec![],
                },
            },
        ),
        (
            1,
            "quota-pin",
            Command::PinTask {
                context: context.clone(),
            },
        ),
    ] {
        let reply = fixture
            .engine
            .apply(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(id).unwrap(),
                expected_revision: Cursor::new(revision),
                command,
            })
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
    }
    let mut pool = pinned.settings.model_access.as_ref().unwrap().pools[0].clone();
    pool.provider_quota_policy = ProviderQuotaPolicy::Strict;
    let result = fixture
        .edit(
            "quota-strict",
            ModelAccessEdit::ReplacePool { pool: pool.clone() },
        )
        .await;
    assert_eq!(result.result, OwnerResult::Applied);
    assert_eq!(result.current_revision.value(), 3);
    let current = resolve_config(&global, &project_path).unwrap();
    assert_ne!(current.id, pinned.id);
    assert_eq!(
        current.settings.model_access.as_ref().unwrap().pools[0],
        pool
    );
    assert_eq!(std::fs::read(&project_path).unwrap(), project_bytes);
    assert!(std::fs::read_to_string(&global).unwrap().contains("strict"));
    let (_, _, records) =
        catalog_page(&fixture.client, ModelAccessCollection::Pools, None, 16).await;
    assert_eq!(
        records,
        vec![ModelAccessRecord::Pool { pool: pool.clone() }]
    );
    let task_config = read(
        &fixture.client,
        OwnerRead::TaskConfig {
            task_id: context.task_id.clone(),
        },
    )
    .await;
    assert!(
        matches!(task_config.data, OwnerData::Domain { outcome } if *outcome == Outcome::Configuration { config: Box::new(pinned.clone()) })
    );
    let root = fixture.close().await;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
        root.path(),
        "ordinary.sock",
        &global,
        &project_path,
        &stopped,
    )
    .await
    .unwrap();
    let client =
        crazyagent::engine::Client::new(root.path().join("ordinary.sock"), Duration::from_secs(2))
            .unwrap();
    let (_, owner, _) = client.with_owner(OwnerClientKind::Tui).await.unwrap();
    let (state, _, records) = catalog_page(&owner, ModelAccessCollection::Pools, None, 16).await;
    assert_eq!(state.config_id, current.id);
    assert_eq!(
        records,
        vec![ModelAccessRecord::Pool { pool: pool.clone() }]
    );
    let task_config = read(
        &owner,
        OwnerRead::TaskConfig {
            task_id: context.task_id,
        },
    )
    .await;
    assert!(
        matches!(task_config.data, OwnerData::Domain { outcome } if *outcome == Outcome::Configuration { config: Box::new(pinned) })
    );
    pool.provider_quota_policy = ProviderQuotaPolicy::BoundedAttempt;
    let (state, source_hash, _) =
        catalog_page(&owner, ModelAccessCollection::Pools, None, 16).await;
    let response = owner
        .request(owner_request(
            "quota-bounded",
            state.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                    edit: ModelAccessEdit::ReplacePool { pool },
                },
            },
        ))
        .await
        .unwrap()
        .response;
    assert!(
        matches!(response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
    );
    assert!(
        !std::fs::read_to_string(&global)
            .unwrap()
            .contains("provider_quota_policy")
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn quota_policy_owner_rejects_stale_config_or_source_without_publication() {
    use crazyagent::config::{ModelAccessEdit, ProviderQuotaPolicy};
    for stale_config in [false, true] {
        let fixture = Fixture::start(true, &quota_policy_catalog()).await;
        let global = fixture.root.path().join("global.toml");
        let old = std::fs::read(&global).unwrap();
        let initial = resolve_config(&global, &fixture.root.path().join("project.toml")).unwrap();
        let mut pool = initial.settings.model_access.as_ref().unwrap().pools[0].clone();
        pool.provider_quota_policy = ProviderQuotaPolicy::Strict;
        let mut request = fixture
            .request("quota-stale", ModelAccessEdit::ReplacePool { pool })
            .await;
        let OwnerOperation::Apply {
            mutation:
                OwnerMutation::ModelAccess {
                    expected_config_id,
                    expected_source_hash,
                    ..
                },
        } = &mut request.operation
        else {
            panic!("model access edit required")
        };
        if stale_config {
            *expected_config_id = crazyagent::protocol::ConfigId::new("stale-config").unwrap();
        } else {
            *expected_source_hash = "0".repeat(64);
        }
        let response = fixture.client.request(request).await.unwrap().response;
        let code = if stale_config {
            "stale_config"
        } else {
            "settings_source_conflict"
        };
        assert!(
            matches!(response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Rejected { code: code.into() } && operation.current_revision.value() == 0)
        );
        assert_eq!(std::fs::read(&global).unwrap(), old);
        assert_eq!(
            resolve_config(&global, &fixture.root.path().join("project.toml")).unwrap(),
            initial
        );
        let root = fixture.close().await;
        let database = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        let publications: i64 = database
            .query_row("SELECT count(*) FROM settings_publication", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(publications, 0);
    }
}

#[tokio::test]
async fn imp1038_owner_round_trips_unassigned_pool_before_projects() {
    use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};

    let fixture = Fixture::start(true, "").await;
    let projects = read(
        &fixture.client,
        OwnerRead::Page {
            view: OwnerView::Projects,
            cursor: None,
            limit: 16,
        },
    )
    .await;
    assert!(matches!(projects.data, OwnerData::Page { items, next: None } if items.is_empty()));
    for (id, edit) in [
        (
            "unassigned-account",
            ModelAccessEdit::AddAccount {
                account_ref: reference("account"),
            },
        ),
        (
            "unassigned-connection",
            ModelAccessEdit::CreateConnection {
                connection: connection(),
            },
        ),
    ] {
        assert_eq!(fixture.edit(id, edit).await.result, OwnerResult::Applied);
    }
    let pool = AccountPool {
        id: reference("unassigned-pool"),
        members: vec![reference("connection")],
        projects: vec![],
        roles: vec![],
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::Strict,
    };
    let receipt = fixture
        .edit("unassigned-pool", ModelAccessEdit::CreatePool { pool: pool.clone() })
        .await;
    let record = read(
        &fixture.client,
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: pool.id.clone(),
        },
    )
    .await;
    let projects = read(
        &fixture.client,
        OwnerRead::Page {
            view: OwnerView::Projects,
            cursor: None,
            limit: 16,
        },
    )
    .await;
    let root = fixture.close().await;
    assert_eq!(receipt.result, OwnerResult::Applied);
    assert_eq!(receipt.current_revision.value(), 3);
    assert!(matches!(
        record.data,
        OwnerData::ModelAccessRecord { record: ModelAccessRecord::Pool { pool: actual }, .. }
            if actual == pool
    ));
    assert!(matches!(projects.data, OwnerData::Page { items, next: None } if items.is_empty()));
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let config = resolve_config(&global, &project).unwrap();
    assert_eq!(config.settings.model_access.as_ref().unwrap().pools, vec![pool.clone()]);
    let engine = Arc::new(
        Engine::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
            .await
            .unwrap(),
    );
    let fixture = Fixture::bind(root, engine, &config).await;
    let record = read(
        &fixture.client,
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: pool.id.clone(),
        },
    )
    .await;
    let root = fixture.close().await;
    assert_eq!(record.revision.value(), 3);
    assert!(matches!(
        record.data,
        OwnerData::ModelAccessRecord { record: ModelAccessRecord::Pool { pool: actual }, .. }
            if actual == pool
    ));
    assert_eq!(resolve_config(&global, &project).unwrap(), config);
    assert!(root.path().join("engine.db").is_file());
}
