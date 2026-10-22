use crazyagent::client_controller::OwnerController;
use crazyagent::config::{
    ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    AccountProfileOutcome, ModelAccessCollection, ModelAccessRecord, OwnerClientKind, OwnerData,
    OwnerRead, ProducerCapability,
};
use crazyagent::protocol::CredentialRef;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn local_none_configuration_uses_real_owner_custody_and_same_request_readback_without_model_execution() {
    let root = tempfile::Builder::new().prefix("ca-profile-").tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(), "e.sock", &global, &root.path().join("p.toml"), &stopped,
    ).await.unwrap();
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { configure_local(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))), "real owner service must stop before propagating assertions");
    result.expect("private profile flow exceeded 20 seconds")
        .expect("private profile assertion failed after cleanup")
        .expect("actual configured profile flow failed");
}

async fn configure_local(root: &Path) -> Result<(), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await.map_err(|error| error.code)?;
    let mut observer = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await.map_err(|error| error.to_string())?;
    let connection = ModelConnection {
        id: CredentialRef::new("local-connection").unwrap(),
        provider: CredentialRef::new("openai").unwrap(),
        harness: CredentialRef::new("rcli").unwrap(),
        protocol: CredentialRef::new("openai-compatible").unwrap(),
        account_ref: CredentialRef::new("local-account").unwrap(),
        endpoint: ModelEndpoint::Http { url: "http://127.0.0.1:11434/v1".into() },
        model: ModelReference { id: "local-model".into(), author: CredentialRef::new("openai").unwrap() },
        auth: ModelAuthentication::None {},
    };
    for (generation, collection, edit) in [
        (1, ModelAccessCollection::Accounts, ModelAccessEdit::AddAccount { account_ref: connection.account_ref.clone() }),
        (2, ModelAccessCollection::Connections, ModelAccessEdit::CreateConnection { connection: connection.clone() }),
    ] {
        let page = bridge.model_access_page(collection, false).await.map_err(|error| error.code)?;
        assert!(page.fresh && page.writable);
        let receipt = bridge.edit_model_access(&page.ticket, edit, generation).await.map_err(|error| error.code)?;
        assert_eq!(receipt.outcome, ActionOutcome::Applied);
        assert!(!receipt.pending);
    }
    let before = observer.read(OwnerRead::ModelAccessRecord {
        collection: ModelAccessCollection::Connections, id: connection.id.clone(),
    }).await.map_err(|error| error.to_string())?;
    assert!(matches!(&before.data, OwnerData::ModelAccessRecord {
        record: ModelAccessRecord::Connection { connection: actual }, writable: true, ..
    } if actual == &connection));
    let inspected = bridge.inspect_model_access(ModelAccessCollection::Connections, connection.id.as_str())
        .await.map_err(|error| error.code)?;
    assert!(inspected.fresh && inspected.writable);
    let view = bridge.configure_account_profile(&inspected.ticket, None, 3).await;
    assert!(view.is_ok(), "real local None configuration must reach owner custody, got {:?}", view.as_ref().err().map(|error| error.code));
    let view = view.map_err(|error| error.code)?;
    assert_eq!(view.ticket, inspected.ticket);
    assert_eq!(view.generation, "3");
    assert!(!view.pending);
    let AccountProfileOutcome::ConfiguredOnly { receipt, historical_replay } = &view.reply.outcome else {
        panic!("actual local profile must be ConfiguredOnly, never an authentication/readiness result");
    };
    assert!(!historical_replay);
    assert_eq!(receipt.connection, connection);
    assert_eq!(receipt.config_id.as_str(), inspected.config_id);
    assert_eq!(receipt.source_hash, inspected.source_hash);
    assert!(!receipt.custody_id.is_empty());
    assert_eq!(receipt.profile_config_sha256.len(), 64);
    assert!(receipt.profile_generation > 0);
    let read = bridge.read_account_profile().await.map_err(|error| error.code)?;
    assert_eq!(read.reply.request_id, view.reply.request_id);
    assert_eq!(read.reply.database_instance, view.reply.database_instance);
    assert_eq!(read.reply.engine_epoch, view.reply.engine_epoch);
    assert_eq!(read.generation, view.generation);
    assert!(!read.pending);
    assert!(matches!(read.reply.outcome, AccountProfileOutcome::ConfiguredOnly {
        receipt: actual, historical_replay: true,
    } if actual == *receipt));
    let fresh = bridge.inspect_model_access(ModelAccessCollection::Connections, connection.id.as_str())
        .await.map_err(|error| error.code)?;
    let capabilities = bridge.account_auth_capabilities(&fresh.ticket).await.map_err(|error| error.code)?;
    assert_eq!(capabilities.connection, connection);
    assert_eq!(capabilities.config_id.as_str(), fresh.config_id);
    assert_eq!(capabilities.source_hash, fresh.source_hash);
    assert_eq!(capabilities.configured, ProducerCapability::Available);
    let after = observer.read(OwnerRead::ModelAccessRecord {
        collection: ModelAccessCollection::Connections, id: connection.id.clone(),
    }).await.map_err(|error| error.to_string())?;
    assert_eq!(after.config_id, before.config_id);
    assert_eq!(after.data, before.data);
    let persisted = std::fs::read_to_string(root.join("g.toml")).map_err(|error| error.to_string())?;
    assert!(persisted.contains("http://127.0.0.1:11434/v1"));
    assert!(!persisted.contains("ConfiguredOnly"));
    assert!(!root.join("p.toml").exists());
    let projects = bridge.projects(false).await.map_err(|error| error.code)?;
    assert!(projects.items.is_empty());
    assert!(bridge.pending_intent().map_err(|error| error.code)?.is_none());
    Ok(())
}
