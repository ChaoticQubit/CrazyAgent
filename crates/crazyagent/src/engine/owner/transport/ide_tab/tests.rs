use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;

const WAIT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ide_tab_reads_the_build_and_never_opens_a_server_before_it_is_ready() {
    let root = tempfile::Builder::new()
        .prefix("ca-idetab-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config_bytes(&format!("timeout_ms = {}\n", WAIT.as_millis()), "")
            .unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
        .await
        .unwrap();
    let client = controller.shared_client().unwrap();

    let macos = cfg!(target_os = "macos");
    let state = client.ide_tab_state(false).await.unwrap();
    assert_eq!(
        state.build,
        BuildState::Missing,
        "a fresh engine root has no build, and reading the state starts no download"
    );
    assert_eq!(
        state.unqualified,
        (!macos).then(|| std::env::consts::OS.to_owned()),
        "only macOS runs the IDE server; elsewhere the tab names the platform"
    );
    assert!(
        !root.path().join("ide").join("build-state.json").exists(),
        "reading the state wrote nothing"
    );

    let project = ProjectId::new("ide-tab-1").unwrap();
    assert!(
        !client.ide_tab_touch(project.clone()).await.unwrap(),
        "no IDE server runs for a project never opened"
    );
    let refused = if macos {
        "ide_build_not_ready"
    } else {
        "ide_platform_unqualified"
    };
    assert_eq!(
        client.ide_tab_open(project.clone()).await,
        Err(EngineError(refused.into())),
        "no server starts before the build is verified, nor on an unqualified platform"
    );
    assert!(
        !root.path().join("ide").join(project.as_str()).exists(),
        "a refused open leaves no server state"
    );
    assert!(IDE_TAB_CODES.contains(&"ide_build_not_ready"));
    assert_eq!(
        public_code(EngineError("rusqlite says no".into())),
        "ide_tab_outcome_unknown",
        "an internal failure reaches the client as one plain code"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tab_request_that_cannot_take_the_owner_barrier_in_time_is_refused_busy() {
    let root = tempfile::Builder::new()
        .prefix("ca-idetab-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let busy = Duration::from_millis(1000);
    let config =
        crate::config::resolve_config_bytes(&format!("timeout_ms = {}\n", busy.as_millis()), "")
            .unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
        .await
        .unwrap();
    let client = controller.shared_client().unwrap();

    let held = service
        .owner_runtime()
        .unwrap()
        .engine
        .owner_egress
        .write()
        .await;
    let asked = std::time::Instant::now();
    assert_eq!(
        client.ide_tab_state(false).await,
        Err(EngineError("owner_busy".into())),
        "a request that cannot take the barrier is refused, not left to eat the open budget"
    );
    assert!(asked.elapsed() >= busy);
    assert!(
        asked.elapsed() < WAIT,
        "the barrier wait is bounded by the engine timeout"
    );
    drop(held);
    assert_eq!(
        client.ide_tab_state(false).await.unwrap().build,
        BuildState::Missing,
        "once the barrier is free the same request is served"
    );
    service.shutdown().await.unwrap();
}
