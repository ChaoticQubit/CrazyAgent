use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelAuthentication, ModelEndpoint};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{ModelAccessCollection, OwnerClientKind, OwnerData, OwnerRead};
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn ordinary_endpoint_preparation_generates_distinct_nonsecret_plans_without_publishing_or_contacting_endpoints()
 {
    let root = tempfile::Builder::new()
        .prefix("ca-profile-plan-")
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
    let mut body = tokio::spawn(async move { prepare_endpoints(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual preparation service must stop before propagating assertions"
    );
    result
        .expect("endpoint preparation exceeded 20 seconds")
        .expect("preparation assertion failed after cleanup")
        .expect("actual endpoint preparation failed");
}

async fn prepare_endpoints(root: &Path) -> Result<(), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    let disconnected = bridge
        .prepare_configured_account("local", "", "http://localhost:11434/v1", "local-model")
        .await
        .unwrap_err();
    assert_eq!(disconnected.code, "not_connected");
    bridge.connect().await.map_err(|error| error.code)?;
    let mut observer = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .map_err(|error| error.to_string())?;
    let local = bridge
        .prepare_configured_account(
            "local",
            " Local ",
            "http://127.0.0.1:11434/v1",
            "local-model",
        )
        .await;
    assert!(
        local.is_ok(),
        "real connected bridge must prepare an ordinary no-key account plan, got {:?}",
        local.as_ref().err().map(|error| error.code)
    );
    let local = local.map_err(|error| error.code)?;
    let keyed = bridge
        .prepare_configured_account(
            "api_key",
            "Hosted",
            "https://api.example.test/v1",
            "hosted-model",
        )
        .await
        .map_err(|error| error.code)?;
    assert_eq!(local.provider, "local");
    assert_eq!(local.label, "Local");
    assert_eq!(keyed.provider, "api_key");
    assert_eq!(keyed.label, "Hosted");
    assert_ne!(local.connection.id, keyed.connection.id);
    assert_ne!(local.connection.account_ref, keyed.connection.account_ref);
    assert_ne!(local.pool.id, keyed.pool.id);
    assert!(matches!(
        local.connection.auth,
        ModelAuthentication::None {}
    ));
    assert!(
        matches!(&keyed.connection.auth, ModelAuthentication::ApiKey { credential_ref } if !credential_ref.as_str().is_empty())
    );
    assert_eq!(
        local.connection.endpoint,
        ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into()
        }
    );
    assert_eq!(
        keyed.connection.endpoint,
        ModelEndpoint::Http {
            url: "https://api.example.test/v1".into()
        }
    );
    for plan in [&local, &keyed] {
        assert_eq!(plan.connection.harness.as_str(), "rcli");
        assert_eq!(plan.connection.protocol.as_str(), "openai-compatible");
        assert_eq!(plan.pool.members, vec![plan.connection.id.clone()]);
        assert!(plan.pool.projects.is_empty() && plan.pool.roles.is_empty());
        assert!(!plan.pool.fallback_allowed);
        let encoded = serde_json::to_value(plan).map_err(|error| error.to_string())?;
        for forbidden in ["secret", "proof", "authenticated", "ready"] {
            assert!(encoded.get(forbidden).is_none());
        }
    }
    for endpoint in [
        "https://localhost/v1",
        "http://example.test/v1",
        "http://0.0.0.0/v1",
        "http://localhost.example.test/v1",
    ] {
        let error = bridge
            .prepare_configured_account("local", "", endpoint, "model")
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_account_endpoint");
    }
    for endpoint in [
        "https://user:password@example.test/v1",
        "https://example.test/v1?key=value",
        "https://example.test/v1#fragment",
        "file:///tmp/model",
    ] {
        let error = bridge
            .prepare_configured_account("api_key", "", endpoint, "model")
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_account_endpoint");
    }
    assert_eq!(
        bridge
            .prepare_configured_account("codex", "", "https://example.test", "model")
            .await
            .unwrap_err()
            .code,
        "invalid_account_provider"
    );
    assert_eq!(
        bridge
            .prepare_configured_account("local", "line\nbreak", "http://localhost/v1", "model")
            .await
            .unwrap_err()
            .code,
        "invalid_account_label"
    );
    assert_eq!(
        bridge
            .prepare_configured_account("local", "", "http://localhost/v1", "")
            .await
            .unwrap_err()
            .code,
        "invalid_account_model"
    );
    for collection in [
        ModelAccessCollection::Accounts,
        ModelAccessCollection::Connections,
        ModelAccessCollection::Pools,
    ] {
        let read = observer
            .read(OwnerRead::ModelAccessPage {
                collection,
                cursor: None,
                limit: 16,
            })
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            matches!(read.data, OwnerData::ModelAccessPage { items, writable: true, .. } if items.is_empty())
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("g.toml")).map_err(|error| error.to_string())?,
        "version = 1\n"
    );
    assert!(!root.join("p.toml").exists());
    assert!(
        bridge
            .projects(false)
            .await
            .map_err(|error| error.code)?
            .items
            .is_empty()
    );
    assert!(
        bridge
            .pending_intent()
            .map_err(|error| error.code)?
            .is_none()
    );
    Ok(())
}
