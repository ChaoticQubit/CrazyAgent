use crazyagent::config::{
    EffectTiming, EffectiveConfig, Origin, SettingDescriptor, resolve_config,
};
use crazyagent::engine::owner::*;
use crazyagent::engine::{Engine, EngineError};
use crazyagent::protocol::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Service {
    engine: Arc<Engine>,
    host: OwnerRuntimeHandle,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), EngineError>>,
}

impl Service {
    async fn start(root: &Path, config: &EffectiveConfig) -> Self {
        config.validate().unwrap();
        let engine = Arc::new(
            Engine::open(&root.join("db"), config.clone())
                .await
                .unwrap(),
        );
        let (server, host) = OwnerServer::bind(root, engine.clone(), config)
            .await
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run(stopped));
        assert!(!task.is_finished());
        Self {
            engine,
            host,
            stop,
            task,
        }
    }

    async fn close(self) {
        self.stop.send(()).unwrap();
        self.task.await.unwrap().unwrap();
        drop(self.host);
        self.engine.close().await.unwrap();
    }
}

fn files() -> (tempfile::TempDir, EffectiveConfig) {
    let root = tempfile::Builder::new()
        .prefix("ca-settings-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        root.path().join("g"),
        "version=1\ntimeout_ms=2500\nqueue_capacity=8\nallowed_destinations=['provider-ref']\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("p"),
        "version=1\nqueue_capacity=3\n[routing]\nfallback_allowed=false\n",
    )
    .unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    assert_eq!(config.settings.queue_capacity, 3);
    assert_eq!(config.settings.timeout_ms, 2500);
    (root, config)
}

fn attach(root: &Path, kind: OwnerClientKind) -> OwnerClient {
    OwnerClient::attach(root, kind, Duration::from_secs(3)).unwrap()
}

fn request(id: &str, query: OwnerRead) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read { query },
    }
}

fn settings_query() -> Result<OwnerRead, serde_json::Error> {
    serde_json::from_value(serde_json::json!({"kind":"Settings"}))
}

async fn actual(client: &OwnerClient, query: OwnerRead, id: &str) -> OwnerReadReply {
    let response = client.request(request(id, query)).await.unwrap();
    let OwnerResponse::Read { state } = response.response else {
        panic!("protected read required")
    };
    assert_eq!(state.revision.value(), 0);
    assert_eq!(state.config_id.as_str().len(), 64);
    state
}

async fn assert_settings(client: &OwnerClient, state: &OwnerReadReply, expected: &EffectiveConfig) {
    let value = serde_json::to_value(&state.data).unwrap();
    assert_eq!(value["kind"], "Settings");
    let names: Vec<String> = serde_json::from_value(value["names"].clone()).unwrap();
    assert_eq!(
        names,
        expected.descriptors.keys().cloned().collect::<Vec<_>>()
    );
    assert_eq!(state.config_id, expected.id);
    for (index, name) in names.iter().enumerate() {
        let query =
            serde_json::from_value(serde_json::json!({"kind":"Setting","name":name})).unwrap();
        let selected = actual(client, query, &format!("setting-{index}")).await;
        assert_eq!(selected.config_id, state.config_id);
        assert_eq!(selected.revision, state.revision);
        let data = serde_json::to_value(&selected.data).unwrap();
        assert_eq!(data["kind"], "Setting");
        let descriptor: SettingDescriptor =
            serde_json::from_value(data["descriptor"].clone()).unwrap();
        assert_eq!(&descriptor, &expected.descriptors[name]);
        assert!(serde_json::to_vec(&selected).unwrap().len() < FRAME_BYTES_MAX - 1024);
    }
}

fn database_state(root: &Path) -> (i64, i64) {
    let db = rusqlite::Connection::open(root.join("db")).unwrap();
    let events = db
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let ledger = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r.get(0))
        .unwrap();
    (events, ledger)
}

#[tokio::test]
async fn actual_native_clients_read_exact_effective_settings_without_mutation() {
    let (root, config) = files();
    let service = Service::start(root.path(), &config).await;
    let query = settings_query();
    if query.is_err() {
        service.close().await;
        panic!("missing protected Settings read: {query:?}");
    }
    let query = query.unwrap();
    let before = database_state(root.path());
    let global = std::fs::read(root.path().join("g")).unwrap();
    let project = std::fs::read(root.path().join("p")).unwrap();
    for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
        let client = attach(root.path(), kind);
        let state = actual(&client, query.clone(), "settings").await;
        assert_settings(&client, &state, &config).await;
        let capabilities = actual(&client, OwnerRead::Capabilities, "caps").await;
        assert!(matches!(
            capabilities.data,
            OwnerData::Capabilities {
                settings_persistence: false,
                ..
            }
        ));
    }
    assert_eq!(config.descriptors["queue_capacity"].origin, Origin::Project);
    assert_eq!(config.descriptors["timeout_ms"].origin, Origin::Global);
    assert_eq!(config.descriptors["clients_max"].origin, Origin::Default);
    assert_eq!(
        config.descriptors["timeout_ms"].effect_timing,
        EffectTiming::Restart
    );
    assert_eq!(database_state(root.path()), before);
    assert_eq!(std::fs::read(root.path().join("g")).unwrap(), global);
    assert_eq!(std::fs::read(root.path().join("p")).unwrap(), project);
    service.close().await;
}

#[tokio::test]
async fn settings_read_tracks_actual_actor_config_and_preserves_snapshot_history() {
    let (root, config) = files();
    let service = Service::start(root.path(), &config).await;
    let query = settings_query().expect("protected settings read must exist");
    let client = attach(root.path(), OwnerClientKind::Desktop);
    service.engine.update_config(config.clone()).await.unwrap();
    let before = database_state(root.path());
    let old = actual(&client, query.clone(), "before").await;
    assert_settings(&client, &old, &config).await;
    std::fs::write(
        root.path().join("p"),
        "version=1\nqueue_capacity=5\n[routing]\nfallback_allowed=true\n",
    )
    .unwrap();
    let next = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    assert_ne!(next.id, config.id);
    service.engine.update_config(next.clone()).await.unwrap();
    let updated = actual(&client, query, "after").await;
    assert_settings(&client, &updated, &next).await;
    assert_ne!(old.config_id, updated.config_id);
    assert_eq!(old.revision, updated.revision);
    let db = rusqlite::Connection::open(root.path().join("db")).unwrap();
    let old_body: String = db
        .query_row(
            "SELECT body FROM configs WHERE id=?1",
            [config.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let saved: EffectiveConfig = serde_json::from_str(&old_body).unwrap();
    assert_eq!(saved, config);
    assert_eq!(database_state(root.path()), before);
    drop(db);
    service.close().await;
}

#[tokio::test]
async fn revoked_and_previous_engine_clients_cannot_read_settings() {
    let (root, config) = files();
    let service = Service::start(root.path(), &config).await;
    let query = settings_query().expect("protected settings read must exist");
    let tui = attach(root.path(), OwnerClientKind::Tui);
    let desktop = attach(root.path(), OwnerClientKind::Desktop);
    assert_settings(&tui, &actual(&tui, query.clone(), "before").await, &config).await;
    let epoch = service.host.epoch(OwnerClientKind::Tui).unwrap();
    let revoked = service
        .host
        .control(OwnerControl {
            operation_id: RequestId::new("revoke-settings").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: epoch,
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    assert!(!revoked.active);
    assert!(
        tui.request(request("revoked", query.clone()))
            .await
            .is_err()
    );
    assert_settings(
        &desktop,
        &actual(&desktop, query.clone(), "desktop-active").await,
        &config,
    )
    .await;
    service.close().await;
    let restarted = Service::start(root.path(), &config).await;
    assert!(
        desktop
            .request(request("old-engine", query.clone()))
            .await
            .is_err()
    );
    let fresh = attach(root.path(), OwnerClientKind::Desktop);
    assert_settings(
        &fresh,
        &actual(&fresh, query.clone(), "fresh-engine").await,
        &config,
    )
    .await;
    let wire = WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("ordinary").unwrap(),
        request_id: RequestId::new("ordinary-settings").unwrap(),
        expected_revision: Cursor::new(0),
        command: Command::Snapshot,
    };
    let mut ordinary = serde_json::to_value(wire).unwrap();
    ordinary["command"] = serde_json::json!({"kind":"Settings"});
    assert!(serde_json::from_value::<WireRequest>(ordinary).is_err());
    restarted.close().await;
}

#[tokio::test]
async fn maximum_valid_settings_remain_inspectable_with_bounded_individual_reads() {
    let (root, _) = files();
    let references: Vec<_> = (0..64)
        .map(|index| format!("{index:03}{}", "a".repeat(125)))
        .collect();
    assert!(references.iter().all(|value| value.len() == 128));
    let array = serde_json::to_string(&references).unwrap();
    let source = format!(
        "version=1\n\
         allowed_destinations={array}\n\
         revoked_destinations={array}\n\
         [routing]\n\
         preference_order={array}\n\
         approved_accounts={array}\n\
         [knowledge]\n\
         processing_destinations={array}\n"
    );
    assert!(source.len() < 65536);
    std::fs::write(root.path().join("g"), source).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    config.validate().unwrap();
    assert!(serde_json::to_vec(&config).unwrap().len() > FRAME_BYTES_MAX);
    let service = Service::start(root.path(), &config).await;
    let client = attach(root.path(), OwnerClientKind::Tui);
    let query = settings_query().expect("protected settings read must exist");
    let catalog = actual(&client, query, "maximum-settings").await;
    assert_settings(&client, &catalog, &config).await;
    for name in [String::new(), "a".repeat(97)] {
        let query =
            serde_json::from_value(serde_json::json!({"kind":"Setting","name":name})).unwrap();
        assert!(
            client
                .request(request("invalid-setting", query))
                .await
                .is_err()
        );
    }
    let unknown = serde_json::from_value(serde_json::json!({
        "kind":"Setting", "name":"unknown_setting"
    }))
    .unwrap();
    let response = client
        .request(request("unknown-setting", unknown))
        .await
        .unwrap();
    let OwnerResponse::Rejected { code } = response.response else {
        panic!("unknown setting rejected")
    };
    assert_eq!(code, "owner_setting_not_found");
    service.close().await;
}
