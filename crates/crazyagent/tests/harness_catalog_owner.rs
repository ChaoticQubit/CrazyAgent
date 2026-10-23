use crazyagent::config::resolve_config;
use crazyagent::engine::Engine;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerServer};
use crazyagent::harnesses::{HarnessStatus, specs};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test]
async fn tui_and_desktop_owners_list_the_engine_harness_catalog() {
    let root = tempfile::Builder::new()
        .prefix("ca-harness-owner-")
        .tempdir_in("/private/tmp")
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
        let entries = client.harness_catalog().await.unwrap();
        let ids: Vec<_> = entries.iter().map(|entry| entry.id.as_str()).collect();
        let expected: Vec<_> = specs().iter().map(|spec| spec.id).collect();
        assert_eq!(ids, expected);
        let codex = entries.iter().find(|entry| entry.id == "codex").unwrap();
        assert_eq!(codex.status, HarnessStatus::Ready);
        assert_eq!(codex.status_label, "Ready");
        assert!(
            entries
                .iter()
                .any(|entry| entry.status_label == "Not installed")
        );
    }
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}
