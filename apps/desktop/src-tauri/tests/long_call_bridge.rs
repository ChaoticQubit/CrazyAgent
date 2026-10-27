use crazyagent::engine::bootstrap::EngineService;
use crazyagent::plugins::{Scope, ServerSpec};
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::Mutex;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn flow(root: &std::path::Path) -> TestResult<()> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/env".into(),
        args: vec!["sleep".into(), "30".into()],
    };
    let server = bridge
        .add_mcp_server(Scope::Global, "silent".into(), spec.clone(), Vec::new())
        .await?;
    bridge.approve_mcp_server(server.id.clone(), spec).await?;
    let shared = Mutex::new(bridge);
    let call = shared.try_lock()?.check_mcp_server(server.id.clone())?;
    let check = tokio::spawn(call);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !check.is_finished(),
        "the real check still waits on the silent server"
    );
    let free = shared.try_lock();
    assert!(
        free.is_ok(),
        "a long engine call leaves the bridge free for other commands"
    );
    let free = free.unwrap();
    assert!(free.status().connected);
    assert!(free.pending_intent()?.is_none());
    assert_eq!(free.plugin_servers(None).await?.len(), 1);
    drop(free);
    let checked = check.await??;
    assert_eq!(checked.id, server.id);
    assert!(checked.tools.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_engine_call_leaves_the_bridge_free_for_other_commands() {
    let root = tempfile::Builder::new()
        .prefix("ca-dlc-")
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
    let path = root.path().to_path_buf();
    let mut body = tokio::spawn(async move { flow(&path).await });
    let result = tokio::time::timeout(Duration::from_secs(40), &mut body).await;
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
