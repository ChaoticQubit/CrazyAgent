use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelAccessEdit, ModelAuthentication, ModelEndpoint};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerClientKind, OwnerData, OwnerRead,
};
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn provider_first_managed_accounts_publish_real_distinct_configuration_without_signing_in() {
    let root = tempfile::Builder::new()
        .prefix("ca-onboard-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { managed_accounts(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))), "actual owner cleanup required");
    result
        .expect("managed account flow exceeded 20 seconds")
        .expect("managed account assertion failed after cleanup")
        .expect("real managed account setup/readback failed");
}

async fn managed_accounts(root: &Path) -> Result<(), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    let disconnected = bridge
        .prepare_managed_account("codex", "Personal")
        .await
        .unwrap_err();
    assert_eq!(disconnected.code, "not_connected");
    bridge.connect().await.map_err(|error| error.code)?;
    let mut observer = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .map_err(|error| error.to_string())?;
    let before = observer
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert!(matches!(&before.data, OwnerData::ModelAccessPage { items, .. } if items.is_empty()));
    let codex = bridge
        .prepare_managed_account("codex", "Personal")
        .await
        .map_err(|error| error.code)?;
    let claude = bridge
        .prepare_managed_account("claude", "Personal")
        .await
        .map_err(|error| error.code)?;
    assert_eq!(codex.provider, "codex");
    assert_eq!(claude.provider, "claude");
    assert_eq!(codex.label, "Personal");
    assert_eq!(claude.label, "Personal");
    assert_ne!(codex.connection.id, claude.connection.id);
    assert_ne!(codex.connection.account_ref, claude.connection.account_ref);
    assert_ne!(codex.pool.id, claude.pool.id);
    assert_eq!(codex.connection.provider.as_str(), "openai");
    assert_eq!(claude.connection.provider.as_str(), "anthropic");
    assert_eq!(codex.connection.harness.as_str(), "codex");
    assert_eq!(claude.connection.harness.as_str(), "claude");
    assert_eq!(codex.connection.protocol.as_str(), "native");
    assert_eq!(claude.connection.protocol.as_str(), "native");
    assert!(matches!(codex.connection.endpoint, ModelEndpoint::Native {}));
    assert!(matches!(claude.connection.endpoint, ModelEndpoint::Native {}));
    let ModelAuthentication::NativeManaged { profile_ref: codex_profile } = &codex.connection.auth else {
        panic!("Codex setup requires a generated managed profile reference");
    };
    let ModelAuthentication::NativeManaged { profile_ref: claude_profile } = &claude.connection.auth else {
        panic!("Claude setup requires a generated managed profile reference");
    };
    assert_ne!(codex_profile, claude_profile);
    assert_eq!(codex.pool.members, vec![codex.connection.id.clone()]);
    assert_eq!(claude.pool.members, vec![claude.connection.id.clone()]);
    assert!(codex.pool.projects.is_empty());
    assert!(claude.pool.projects.is_empty());
    assert!(codex.pool.roles.is_empty());
    assert!(claude.pool.roles.is_empty());
    let encoded = serde_json::to_value(&codex).map_err(|error| error.to_string())?;
    assert!(encoded.get("authenticated").is_none());
    assert!(encoded.get("ready").is_none());
    assert!(encoded.get("proof").is_none());
    assert!(encoded.get("secret").is_none());
    let after = observer
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(before.revision, after.revision);
    assert!(matches!(&after.data, OwnerData::ModelAccessPage { items, .. } if items.is_empty()));
    assert!(bridge.pending_intent().map_err(|error| error.code)?.is_none());
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Account { account_ref: codex.connection.account_ref.clone() },
        ModelAccessEdit::AddAccount { account_ref: codex.connection.account_ref.clone() },
        1,
    ).await?;
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Connection { connection: codex.connection.clone() },
        ModelAccessEdit::CreateConnection { connection: codex.connection.clone() },
        2,
    ).await?;
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Pool { pool: codex.pool.clone() },
        ModelAccessEdit::CreatePool { pool: codex.pool.clone() },
        3,
    ).await?;
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Account { account_ref: claude.connection.account_ref.clone() },
        ModelAccessEdit::AddAccount { account_ref: claude.connection.account_ref.clone() },
        4,
    ).await?;
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Connection { connection: claude.connection.clone() },
        ModelAccessEdit::CreateConnection { connection: claude.connection.clone() },
        5,
    ).await?;
    publish_record(
        &mut bridge,
        &mut observer,
        ModelAccessRecord::Pool { pool: claude.pool.clone() },
        ModelAccessEdit::CreatePool { pool: claude.pool.clone() },
        6,
    ).await?;
    let pools = bridge.model_access_page(ModelAccessCollection::Pools, false)
        .await.map_err(|error| error.code)?;
    assert_eq!(pools.items.len(), 2);
    assert!(!pools.has_more);
    let persisted = std::fs::read_to_string(root.join("g.toml")).map_err(|error| error.to_string())?;
    assert!(persisted.contains(codex.connection.account_ref.as_str()));
    assert!(persisted.contains(claude.connection.account_ref.as_str()));
    assert!(!root.join("p.toml").exists());
    assert!(bridge.pending_intent().map_err(|error| error.code)?.is_none());
    Ok(())
}

async fn publish_record(
    bridge: &mut NativeBridge,
    observer: &mut OwnerController,
    expected: ModelAccessRecord,
    edit: ModelAccessEdit,
    generation: u64,
) -> Result<(), String> {
    let collection = match &expected {
        ModelAccessRecord::Account { .. } => ModelAccessCollection::Accounts,
        ModelAccessRecord::Connection { .. } => ModelAccessCollection::Connections,
        ModelAccessRecord::Pool { .. } => ModelAccessCollection::Pools,
    };
    let page = bridge.model_access_page(collection, false).await.map_err(|error| error.code)?;
    assert!(page.fresh);
    assert!(page.writable);
    let receipt = bridge.edit_model_access(&page.ticket, edit, generation)
        .await.map_err(|error| error.code)?;
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert_eq!(receipt.intent.generation, generation.to_string());
    assert!(!receipt.pending);
    let reply = observer.read(OwnerRead::ModelAccessRecord {
        collection,
        id: expected.id().clone(),
    }).await.map_err(|error| error.to_string())?;
    let OwnerData::ModelAccessRecord { record, writable, .. } = reply.data else {
        return Err("actual independent owner record required".into());
    };
    assert!(writable);
    assert_eq!(record, expected);
    let inspected = bridge.inspect_model_access(collection, expected.id().as_str())
        .await.map_err(|error| error.code)?;
    assert!(inspected.fresh);
    assert!(inspected.writable);
    assert_eq!(inspected.record, expected);
    assert_eq!(inspected.revision, reply.revision.value().to_string());
    Ok(())
}
