#![cfg(target_os = "macos")]

use crazyagent::setup::macos::{GRAPHIFY_VERSION, MacInstaller, UV_VERSION, pin};
use crazyagent::setup::{Dependency, Installer};
use std::os::unix::fs::PermissionsExt;

#[test]
fn network_installs_pinned_uv_and_graphify_into_the_app_data_folder() {
    let root = tempfile::Builder::new()
        .prefix("ca-setup-net-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    assert_eq!(installer.find(Dependency::Uv).unwrap(), None);

    let uv = installer
        .install(Dependency::Uv, &pin(Dependency::Uv))
        .unwrap();
    assert_eq!(uv.as_str(), UV_VERSION);
    assert!(root.path().join("setup/bin/uv").is_file());

    let graphify = installer
        .install(Dependency::Graphify, &pin(Dependency::Graphify))
        .unwrap();
    assert_eq!(graphify.as_str(), GRAPHIFY_VERSION);
    assert!(root.path().join("setup/bin/graphify").exists());
    assert!(root.path().join("setup/uv/tools").is_dir());
}

#[tokio::test]
async fn network_run_setup_installs_a_ticked_skill_entry_through_the_engine() {
    use crazyagent::client_controller::OwnerController;
    use crazyagent::engine::bootstrap::EngineService;
    use crazyagent::engine::owner::OwnerClientKind;
    use crazyagent::setup::PackState;
    use std::time::{Duration, Instant};

    let root = tempfile::Builder::new()
        .prefix("ca-setup-run-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(
        &global,
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n",
    )
    .unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let owner = OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();

    let started = owner.run_setup(vec!["caveman".into()]).await.unwrap();
    assert!(started.running);
    let deadline = Instant::now() + Duration::from_secs(1800);
    let status = loop {
        let status = owner.setup_status().await.unwrap();
        if !status.running {
            break status;
        }
        assert!(Instant::now() < deadline, "setup run deadline");
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let state = |id: &str| {
        status
            .pack
            .iter()
            .find(|entry| entry.id == id)
            .unwrap()
            .state
            .clone()
    };
    assert_eq!(state("caveman"), PackState::On);
    assert_eq!(state("superpowers"), PackState::Available);
    let skills = owner.plugin_skills(None).await.unwrap();
    assert!(skills.iter().any(|skill| skill.name == "caveman"));
    assert!(root.path().join("setup/pack").is_dir());

    let off = owner.set_pack_entry("caveman".into(), false).await.unwrap();
    let caveman = off.pack.iter().find(|entry| entry.id == "caveman").unwrap();
    assert_eq!(caveman.state, PackState::Off);
    let removed = owner.remove_pack_entry("caveman".into()).await.unwrap();
    let caveman = removed
        .pack
        .iter()
        .find(|entry| entry.id == "caveman")
        .unwrap();
    assert_eq!(caveman.state, PackState::Available);
    drop(owner);
    service.shutdown().await.unwrap();
}
