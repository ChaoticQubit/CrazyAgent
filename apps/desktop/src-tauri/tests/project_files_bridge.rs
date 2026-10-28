#![cfg(unix)]

mod engine_support;

use crazyagent::engine::bootstrap::EngineService;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::ProjectSpec;
use crazyagent_desktop::bridge::{ActionOutcome, BridgeQueue, NativeBridge};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Sets up one project through the desktop bridge, as New Project does.
/// Input: the connected bridge with a projects home. Output: the project id and folder.
async fn plant_shop(bridge: &mut NativeBridge) -> Result<(String, PathBuf), Box<dyn Error>> {
    let spec = ProjectSpec {
        name: "Plant shop".into(),
        repositories: vec![],
    };
    let setup = ProjectSetup {
        about: "Sells plants.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let created = bridge.setup_project(spec, setup, 2).await?;
    assert!(
        matches!(created.outcome, ActionOutcome::Applied),
        "{created:?}"
    );
    let id = created.intent.project_id.clone().ok_or("no project id")?;
    let folder = bridge
        .project_setup(&id)
        .await?
        .ok_or("no stored setup")?
        .project_folder();
    Ok((id, folder))
}

/// Asks for a change while an `@` listing is held inside the real git: the folder's
/// `notes/.gitignore` is a pipe git waits on until the test opens it.
/// Input: the engine folder. Output: none, or the first failed step.
async fn flow(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
    assert!(bridge.connect().await?.connected);
    let _home = engine_support::pick_home(&mut bridge, 1).await;
    let (id, folder) = plant_shop(&mut bridge).await?;
    let notes = folder.join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(notes.join("watering-plan.txt"), "Water on Mondays.\n")?;
    let gate = notes.join(".gitignore");
    let made = crazyagent::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&gate)
            .env_clear(),
    )?;
    assert!(made.success(), "mkfifo failed");
    let queue = Arc::new(BridgeQueue::new(bridge));
    let listing = tokio::spawn({
        let queue = Arc::clone(&queue);
        async move { queue.project_files(&id, "watering").await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let change = queue.enter().await.map(drop).map_err(|e| e.code);
    assert!(
        !listing.is_finished(),
        "the listing still waits on git, held at the ignore pipe"
    );
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&gate)?;
    let listed = tokio::time::timeout(Duration::from_secs(10), listing).await??;
    drop(opened);
    assert_eq!(
        change,
        Ok(()),
        "a change asked while a slow @ listing runs gets the bridge instead of busy"
    );
    assert_eq!(listed.map_err(|e| e.code)?, ["notes/watering-plan.txt"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_asked_while_a_slow_at_listing_runs_is_not_refused_busy() {
    let root = tempfile::Builder::new()
        .prefix("ca-files-desk-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = engine_support::engine_config(root.path());
    let (_cancel, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), flow(root.path())).await;
    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "actual owner service must stop"
    );
    result.unwrap().unwrap();
}
