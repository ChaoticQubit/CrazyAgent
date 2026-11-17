#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerMutation};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Starts an engine in a private temporary folder that holds no 2.4 backup.
/// Input: none. Output: the folder, the running engine and the sender that keeps it running.
async fn engine() -> (
    tempfile::TempDir,
    EngineService,
    tokio::sync::watch::Sender<bool>,
) {
    let root = tempfile::Builder::new()
        .prefix("ca-restore-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    fs::write(&global, "timeout_ms = 5000\n").unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    assert!(root.path().join("engine.db").is_file());
    (root, service, stop)
}

/// A restore stops the engine and replaces the journal that would confirm it, so a kept
/// restore checked after a restart reads as never recorded. Sending it again then would
/// restore a second time without the owner's confirm: a restore whose send was cut off is
/// dropped, never sent again.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_whose_send_was_cut_off_is_never_sent_again() {
    let (root, mut service, _stop) = engine().await;
    let mut owner = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    owner.setup_status().await.unwrap();
    owner
        .begin_mutation(OwnerMutation::RestorePinned, 1)
        .unwrap();
    let cut = tokio::time::timeout(Duration::ZERO, owner.send_pending()).await;
    assert!(cut.is_err(), "the send is cut off before its answer");
    assert!(owner.pending().is_some_and(|kept| kept.uncertain()));
    let again = owner.retry_pending().await.unwrap_err();
    assert_eq!(again.0, "controller_restore_not_resent");
    assert!(owner.pending().is_none(), "the cut-off restore is dropped");
    let refused = owner
        .begin_mutation(OwnerMutation::RestorePinned, 2)
        .unwrap_err();
    assert_ne!(refused.0, "controller_pending_intent");
    service.shutdown().await.unwrap();
}
