#![cfg(unix)]

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::plugins::{Scope, ServerSpec};
use crazyagent_desktop::bridge::NativeBridge;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::Mutex;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const IMPORT_TEST: &str = "importing_skills_and_adding_a_server_leave_the_bridge_free";
const PLANTED_ROOT: &str = "CRAZYAGENT_TEST_PLANTED_HOME_ROOT";

async fn flow(root: &std::path::Path) -> TestResult<()> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/env".into(),
        args: vec!["sleep".into(), "30".into()],
    };
    let server = bridge
        .add_mcp_server(Scope::Global, "silent".into(), spec.clone(), Vec::new())?
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

async fn import_flow(root: &std::path::Path) -> TestResult<()> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    bridge.connect().await?;
    let found = root.join("home/.claude/skills/style");
    std::fs::create_dir_all(&found)?;
    std::fs::write(
        found.join("SKILL.md"),
        "---\nname: style\ndescription: House style.\n---\n",
    )?;
    let shared = Mutex::new(bridge);
    let scan = tokio::spawn(shared.try_lock()?.skill_import_scan()?);
    let import = tokio::spawn(
        shared
            .try_lock()?
            .import_skill(found.display().to_string(), None)?,
    );
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/env".into(),
        args: vec!["true".into()],
    };
    let add = tokio::spawn(shared.try_lock()?.add_mcp_server(
        Scope::Global,
        "quiet".into(),
        spec,
        vec![("QUIET_TOKEN".into(), "dummy-value".into())],
    )?);
    let free = shared.try_lock();
    assert!(
        free.is_ok(),
        "the import scan, the import and the server add leave the bridge free while they run"
    );
    let free = free.unwrap();
    assert!(free.status().connected);
    assert!(free.pending_intent()?.is_none());
    drop(free);
    let list = scan.await??;
    assert!(list.found.iter().any(|candidate| candidate.name == "style"));
    let imported = import.await??;
    assert_eq!(imported.skill.name, "style");
    let server = add.await??;
    assert_eq!(server.status_label, "Waiting for approval");
    assert!(server.secrets[0].saved);
    let free = shared.try_lock()?;
    assert_eq!(free.plugin_servers(None).await?.len(), 1);
    assert_eq!(free.plugin_skills(None).await?.len(), 1);
    Ok(())
}

async fn run_with_planted_home() {
    let root = tempfile::Builder::new()
        .prefix("ca-dli-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let planted = root.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        crazyagent::spawn_gate::output(
            std::process::Command::new(exe)
                .args([IMPORT_TEST, "--exact", "--nocapture"])
                .env("HOME", planted.join("home"))
                .env(PLANTED_ROOT, &planted)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(&format!("test {IMPORT_TEST} ... ok")),
        "the flow with the planted home as HOME failed: {}\n{stdout}\n{stderr}",
        output.status
    );
}

async fn import_with_planted_home(root: &std::path::Path) {
    assert_eq!(
        std::env::var_os("HOME").map(std::path::PathBuf::from),
        Some(root.join("home")),
        "the engine in this run scans the planted home, never the owner's"
    );
    let global = root.join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root,
        "e.sock",
        &global,
        &root.join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let path = root.to_path_buf();
    let mut body = tokio::spawn(async move { import_flow(&path).await });
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn importing_skills_and_adding_a_server_leave_the_bridge_free() {
    match std::env::var_os(PLANTED_ROOT) {
        Some(root) => import_with_planted_home(std::path::Path::new(&root)).await,
        None => run_with_planted_home().await,
    }
}
