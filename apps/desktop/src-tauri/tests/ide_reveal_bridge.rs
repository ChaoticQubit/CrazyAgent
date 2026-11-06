#![cfg(unix)]

#[path = "engine_support/config.rs"]
mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent_desktop::bridge::NativeBridge;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 1] = ["desktop_ide_focus"];

#[test]
fn ide_focus_command_is_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let client = include_str!("../../src/engine-client.ts");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("fn {command}(")),
            "{command} binding"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} handler"
        );
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&permission).count(),
            1,
            "{command} capability"
        );
        assert_eq!(
            client.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} client"
        );
    }
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert_eq!(
        bridge.ide_focus().err().map(|error| error.code),
        Some("not_connected")
    );
    assert!(bridge.connect().await?.connected);
    assert_eq!(bridge.ide_focus()?.await?, None);
    assert_eq!(bridge.ide_focus()?.await?, None);
    Ok(())
}

#[tokio::test]
async fn desktop_reads_no_ide_focus_when_no_reveal_waits() {
    let root = tempfile::Builder::new()
        .prefix("ca-ide-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    result.unwrap().unwrap();
}
