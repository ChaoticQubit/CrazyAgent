#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::ProjectSpec;
use crazyagent::roadmap_status::Refresh;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const COMMANDS: [&str; 3] = [
    "desktop_roadmap_status",
    "desktop_refresh_roadmap_status",
    "desktop_set_roadmap_refresh",
];

#[test]
fn roadmap_status_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-projects.ts");
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
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let receipt = bridge
        .setup_project(
            ProjectSpec {
                name: "Recipe box".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Keeps family recipes.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Website],
                research: vec![ResearchKind::MarketResearch],
                pool: None,
                team: None,
                documents: vec![],
            },
            2,
        )
        .await?;
    assert!(
        matches!(receipt.outcome, ActionOutcome::Applied),
        "{receipt:?}"
    );
    let id = receipt.intent.project_id.clone().unwrap();

    let first = bridge.roadmap_status(id.clone())?.await?;
    assert!(first.items.is_empty() && first.checked_ms.is_none());
    assert_eq!(first.refresh, Refresh::AfterLanding);
    assert!(!first.stale && !first.running);

    for (name, saved) in [
        ("refresh_only", Refresh::RefreshOnly),
        ("after_landing", Refresh::AfterLanding),
        ("off", Refresh::Off),
    ] {
        bridge
            .set_roadmap_refresh(id.clone(), name.to_owned())?
            .await?;
        assert_eq!(bridge.roadmap_status(id.clone())?.await?.refresh, saved);
    }
    let refused = bridge.refresh_roadmap_status(id.clone())?.await;
    assert_eq!(refused.unwrap_err().code, "roadmap_status_off");
    let off = bridge.roadmap_status(id.clone())?.await?;
    assert!(!off.stale && !off.running, "{off:?}");

    bridge
        .set_roadmap_refresh(id.clone(), "refresh_only".to_owned())?
        .await?;
    bridge.refresh_roadmap_status(id.clone())?.await?;
    let asked = bridge.roadmap_status(id.clone())?.await?;
    assert!(
        asked.stale || asked.running || asked.problem.is_some(),
        "a Refresh leaves a check due, running or ended: {asked:?}"
    );

    let unknown_choice = bridge.set_roadmap_refresh(id.clone(), "sometimes".to_owned());
    assert_eq!(
        unknown_choice.err().map(|error| error.code),
        Some("roadmap_status_invalid")
    );
    let missing = bridge.roadmap_status("no-such-project".to_owned())?.await;
    assert_eq!(missing.unwrap_err().code, "roadmap_status_project_missing");
    Ok(())
}

#[tokio::test]
async fn desktop_reads_refreshes_and_sets_the_roadmap_status_through_the_real_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-desk-rs-")
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
