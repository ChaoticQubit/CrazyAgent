#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::OwnerClientKind;
use crazyagent::setup::pack::{PackGroup, default_pack};
use crazyagent::setup::{Dependency, DependencyState, PackState};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-setup-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service: Some(service),
            _stop: stop,
        }
    }

    async fn controller(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
    }

    async fn close(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn setup_status_lists_the_dependencies_and_the_shipped_pack() {
    let case = Case::new().await;
    let owner = case.controller().await;

    let status = owner.setup_status().await.unwrap();
    assert!(!status.running);
    let names: Vec<Dependency> = status.dependencies.iter().map(|dep| dep.name).collect();
    assert_eq!(names, Dependency::ALL.to_vec());
    for dependency in &status.dependencies {
        assert!(
            matches!(
                dependency.state,
                DependencyState::Found { .. } | DependencyState::Missing
            ),
            "{dependency:?}"
        );
    }
    let manifest = default_pack().unwrap();
    let ids: Vec<&str> = status.pack.iter().map(|entry| entry.id.as_str()).collect();
    let shipped: Vec<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, shipped);
    assert!(
        status
            .pack
            .iter()
            .all(|entry| entry.state == PackState::Available)
    );
    let graphify = status
        .pack
        .iter()
        .find(|entry| entry.id == "graphify")
        .unwrap();
    assert_eq!(graphify.group, PackGroup::Always);
    assert_eq!(graphify.source, "github.com/safishamsi/graphify");

    case.close().await;
}

#[tokio::test]
async fn required_unknown_and_uninstalled_entries_are_refused_plainly() {
    let case = Case::new().await;
    let owner = case.controller().await;

    let code = |result: Result<_, crazyagent::engine::EngineError>| result.err().unwrap().0;
    assert_eq!(
        code(owner.remove_pack_entry("graphify".into()).await),
        "pack_required"
    );
    assert_eq!(
        code(owner.set_pack_entry("graphify".into(), false).await),
        "pack_required"
    );
    assert_eq!(
        code(owner.set_pack_entry("superpowers".into(), false).await),
        "pack_not_installed"
    );
    assert_eq!(
        code(owner.run_setup(vec!["no-such-entry".into()]).await),
        "pack_unknown"
    );
    assert_eq!(
        code(owner.set_pack_entry("bad id!".into(), true).await),
        "pack_unknown"
    );
    assert!(!owner.setup_status().await.unwrap().running);

    case.close().await;
}
