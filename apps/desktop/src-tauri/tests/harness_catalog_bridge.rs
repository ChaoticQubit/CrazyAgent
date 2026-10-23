use crazyagent::engine::bootstrap::EngineService;
use crazyagent::harnesses::{HarnessStatus, specs};
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tauri::ipc::Origin;

async fn flow(root: &Path) -> Result<(), String> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert_eq!(
        bridge.harness_catalog().await.unwrap_err().code,
        "not_connected"
    );
    assert!(
        bridge
            .connect()
            .await
            .map_err(|e| e.code.to_owned())?
            .connected
    );
    let entries = bridge
        .harness_catalog()
        .await
        .map_err(|e| e.code.to_owned())?;
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    let expected: Vec<&str> = specs().iter().map(|spec| spec.id).collect();
    assert_eq!(ids, expected);
    let codex = entries.iter().find(|entry| entry.id == "codex").unwrap();
    assert_eq!(codex.status, HarnessStatus::Ready);
    assert_eq!(codex.status_label, "Ready");
    assert!(
        entries
            .iter()
            .any(|entry| entry.status_label == "Not installed")
    );
    Ok(())
}

#[tokio::test]
async fn the_desktop_bridge_lists_the_engine_harness_catalog() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-harness-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
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
    let mut body = tokio::spawn(async move { flow(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual owner service must stop"
    );
    result.unwrap().unwrap().unwrap();
}

#[test]
fn the_harness_catalog_command_uses_the_local_main_window_acl() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    let command = "desktop_harness_catalog";
    assert!(
        authority
            .resolve_access(command, "main", "main", &Origin::Local)
            .is_some()
    );
    assert!(
        authority
            .resolve_access(command, "other", "other", &Origin::Local)
            .is_none()
    );
    assert!(
        authority
            .resolve_access(
                command,
                "main",
                "main",
                &Origin::Remote {
                    url: "https://example.test".parse().unwrap(),
                },
            )
            .is_none()
    );
}
