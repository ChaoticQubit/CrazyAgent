#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::engine::Engine;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerServer};
use crazyagent::harnesses::{HarnessEntry, HarnessStatus, TeamUse, specs};
use crazyagent::team::launch_harness;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test]
async fn tui_and_desktop_owners_list_the_engine_harness_catalog() {
    let root = tempfile::Builder::new()
        .prefix("ca-harness-owner-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, _host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(stopped));
    for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
        let client = OwnerClient::attach(root.path(), kind, Duration::from_secs(5)).unwrap();
        let entries = client
            .harness_catalog(kind == OwnerClientKind::Desktop)
            .await
            .unwrap();
        let ids: Vec<_> = entries.iter().map(|entry| entry.id.as_str()).collect();
        let expected: Vec<_> = specs().iter().map(|spec| spec.id).collect();
        assert_eq!(ids, expected);
        let codex = entries.iter().find(|entry| entry.id == "codex").unwrap();
        assert_eq!(codex.status, HarnessStatus::Ready);
        assert_eq!(codex.status_label, "Ready");
        assert_eq!(codex.team, TeamUse::Available);
        for entry in &entries {
            assert_eq!(
                entry.team == TeamUse::Available,
                launch_harness(&entry.id).is_some(),
                "{kind:?} {}",
                entry.id
            );
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry.status_label == "Not installed")
        );
    }
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_tui_controller_lists_the_harness_catalog_from_a_discovered_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-harness-tui-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
    let controller =
        OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
            .await
            .unwrap();
    let entries = controller.harness_catalog(false).await;
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let entries = entries.unwrap();
    let labels: Vec<_> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.status_label.as_str()))
        .collect();
    assert_eq!(labels.len(), specs().len());
    assert_eq!(labels[0], ("Codex", "Ready"));
    assert!(labels.iter().all(|(_, label)| {
        *label == "Ready" || *label == "Not installed" || label.starts_with("Limited control: ")
    }));
}

#[tokio::test]
async fn an_owner_attaching_after_a_claude_update_sees_the_new_install() {
    let root = tempfile::Builder::new()
        .prefix("ca-harness-update-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
    let host = root.path().join("crazyagent-pin-only");
    let old = root.path().join("cask/2.1.309/claude");
    let new = root.path().join("cask/2.1.322/claude");
    let link = root.path().join("bin/claude");
    for path in [&host, &old, &new] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, path.as_os_str().as_encoded_bytes()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&old, &link).unwrap();
    service.register_native_auth_host(&host).unwrap();
    service.register_claude_executable(&link).unwrap();
    service.ensure_ready(&stopped).unwrap();
    let claude = |entries: Vec<HarnessEntry>| {
        entries
            .into_iter()
            .find(|entry| entry.id == "claude-code")
            .and_then(|entry| entry.executable)
    };
    let before = OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        claude(before.harness_catalog(false).await.unwrap()),
        Some(old.clone())
    );
    std::fs::remove_dir_all(old.parent().unwrap()).unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&new, &link).unwrap();
    let reopened =
        OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
            .await
            .unwrap();
    let entries = reopened.harness_catalog(false).await;
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claude(entries.unwrap()), Some(new));
}
