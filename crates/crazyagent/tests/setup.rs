#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::setup::pack::{PackGroup, default_pack};
use crazyagent::setup::{Dependency, DependencyState, PackState, SetupStatus, Version};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const DEFAULT_DEADLINE: &str = "version = 1\ntimeout_ms = 1000\nallowed_destinations = []\n";

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: watch::Sender<bool>,
    _stopped: watch::Receiver<bool>,
}

impl Case {
    async fn new() -> Self {
        Self::with_settings(SETTINGS).await
    }

    async fn with_settings(settings: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-setup-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, settings).unwrap();
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
            _stopped: stopped,
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

fn result(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            operation.result
        }
        other => panic!("actual owner receipt required: {other:?}"),
    }
}

fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

async fn settled(owner: &mut OwnerController) -> SetupStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = owner.setup_status().await.unwrap();
        let checking = status
            .dependencies
            .iter()
            .any(|dependency| dependency.state == DependencyState::Checking);
        if !checking {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "tools still checking: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn setup_status_lists_the_dependencies_and_the_shipped_pack() {
    let case = Case::new().await;
    let mut owner = case.controller().await;

    let status = settled(&mut owner).await;
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
    let mut owner = case.controller().await;

    assert_eq!(
        result(owner.remove_pack_entry("graphify".into(), 1).await.unwrap()),
        rejected("pack_required")
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("graphify".into(), false, 1)
                .await
                .unwrap()
        ),
        rejected("pack_required")
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("superpowers".into(), false, 1)
                .await
                .unwrap()
        ),
        rejected("pack_not_installed")
    );
    assert_eq!(
        result(
            owner
                .run_setup(vec!["no-such-entry".into()], 1)
                .await
                .unwrap()
        ),
        rejected("pack_unknown")
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("bad id!".into(), true, 1)
                .await
                .unwrap()
        ),
        rejected("pack_unknown")
    );
    assert!(!owner.setup_status().await.unwrap().running);

    case.close().await;
}

fn graphify(status: &SetupStatus) -> DependencyState {
    status
        .dependencies
        .iter()
        .find(|dependency| dependency.name == Dependency::Graphify)
        .unwrap()
        .state
        .clone()
}

#[tokio::test]
async fn a_version_check_slower_than_the_reply_deadline_does_not_fail_the_status_read() {
    let case = Case::new().await;
    let setup = case.root.path().join("setup");
    let bin = setup.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o700)).unwrap();
    let program = bin.join("graphify");
    fs::write(&program, "#!/bin/sh\n/bin/sleep 4\necho graphify 0.9.9\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let mut owner = case.controller().await;

    let asked = Instant::now();
    let first = owner.setup_status().await.unwrap();
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(graphify(&first), DependencyState::Checking);
    let checked = settled(&mut owner).await;
    assert_eq!(
        graphify(&checked),
        DependencyState::Found {
            version: Version::parse("0.9.9").unwrap()
        }
    );

    case.close().await;
}

#[tokio::test]
async fn repeated_status_reads_at_the_default_deadline_all_answer() {
    let case = Case::with_settings(DEFAULT_DEADLINE).await;
    let mut owner = case.controller().await;

    let mut failures = Vec::new();
    let mut slowest = Duration::ZERO;
    for read in 0..20 {
        let asked = Instant::now();
        let outcome = owner.setup_status().await;
        let took = asked.elapsed();
        slowest = slowest.max(took);
        if let Err(error) = outcome {
            failures.push(format!(
                "read {read}: {} after {} ms",
                error.0,
                took.as_millis()
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
    assert!(slowest < Duration::from_secs(1), "{slowest:?}");

    case.close().await;
}
