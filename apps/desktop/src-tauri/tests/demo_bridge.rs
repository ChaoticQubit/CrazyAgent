#![cfg(unix)]

mod engine_support;

use crazyagent::demo::{GOAL, PROJECT_ID};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 2] = ["desktop_project_demo", "desktop_create_demo"];

#[test]
fn demo_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-demo.ts");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("async fn {command}(")),
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
            window.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} window"
        );
    }
}

async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    let unconnected = bridge.project_demo().await;
    assert_eq!(unconnected.unwrap_err().code, "not_connected");
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;

    let first = bridge.project_demo().await?;
    assert_eq!(first.project_id, None);
    assert_eq!(first.created_ms, None);
    assert_eq!(first.goal, GOAL);
    assert!(!first.steps.created && !first.steps.goal_sent);
    assert!(!first.steps.agent_started && !first.steps.question_answered);
    assert!(!first.steps.change_set_accepted);
    assert!(
        first
            .pools
            .iter()
            .all(|pool| pool.pool.as_str() != "pool-absent"),
        "{first:?}"
    );

    let blank = bridge.create_demo(String::new(), 2).await;
    assert_eq!(blank.unwrap_err().code, "demo_pool_missing");

    let refused = bridge.create_demo("pool-absent".into(), 3).await?;
    assert_eq!(refused.intent.kind, "create_demo");
    assert!(
        matches!(refused.outcome, ActionOutcome::Rejected),
        "{refused:?}"
    );
    assert_eq!(refused.rejection.as_deref(), Some("demo_pool_missing"));
    assert!(!refused.pending);

    let after = bridge.project_demo().await?;
    assert_eq!(after.project_id, None, "a refused demo makes no project");
    let listed = bridge.projects(false).await?;
    assert!(listed.items.iter().all(|row| row.id != PROJECT_ID));
    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_reads_the_demo_and_a_create_on_a_gone_pool_is_refused_by_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-demo-")
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
