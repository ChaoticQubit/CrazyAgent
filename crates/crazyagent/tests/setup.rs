#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::plugins::Scope;
use crazyagent::setup::pack::{PackGroup, default_pack};
use crazyagent::setup::{Dependency, DependencyState, PackEntry, PackState, SetupStatus, Version};
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
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
                DependencyState::Found { .. }
                    | DependencyState::Bundled { .. }
                    | DependencyState::System { .. }
                    | DependencyState::Missing { .. }
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
    assert_eq!(graphify.group, PackGroup::Graph);
    assert_eq!(graphify.source, "github.com/safishamsi/graphify");

    case.close().await;
}

fn entry<'a>(status: &'a SetupStatus, id: &str) -> &'a PackEntry {
    status.pack.iter().find(|entry| entry.id == id).unwrap()
}

#[tokio::test]
async fn graphify_already_in_the_plugin_store_reads_installed_and_switches_like_any_entry() {
    let case = Case::new().await;
    let mut owner = case.controller().await;
    let owned = tempfile::Builder::new()
        .prefix("ca-setup-owned-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let folder = owned.path().canonicalize().unwrap().join("graphify");
    fs::create_dir(&folder).unwrap();
    fs::write(
        folder.join("SKILL.md"),
        "---\nname: graphify\ndescription: Reads the project's code graph.\n---\n",
    )
    .unwrap();
    let skill = owner
        .add_skill(Scope::Global, folder.to_str().unwrap().into())
        .await
        .unwrap();

    let status = settled(&mut owner).await;
    assert_eq!(entry(&status, "graphify").state, PackState::On);
    assert_eq!(
        result(
            owner
                .set_pack_entry("graphify".into(), false, 0, 1)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        entry(&owner.setup_status().await.unwrap(), "graphify").state,
        PackState::Off
    );
    let skills = owner.plugin_skills(None).await.unwrap();
    assert!(
        !skills
            .iter()
            .find(|item| item.id == skill.id)
            .unwrap()
            .enabled
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("graphify".into(), true, 1, 2)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        entry(&owner.setup_status().await.unwrap(), "graphify").state,
        PackState::On
    );
    assert_eq!(
        result(
            owner
                .remove_pack_entry("graphify".into(), 2, 3)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        entry(&owner.setup_status().await.unwrap(), "graphify").state,
        PackState::Available
    );
    assert!(
        owner
            .plugin_skills(None)
            .await
            .unwrap()
            .iter()
            .all(|item| item.name != "graphify")
    );

    case.close().await;
}

#[tokio::test]
async fn no_entry_is_forced_and_unknown_or_uninstalled_entries_are_refused_plainly() {
    let case = Case::new().await;
    let mut owner = case.controller().await;

    assert_eq!(
        result(
            owner
                .remove_pack_entry("graphify".into(), 0, 1)
                .await
                .unwrap()
        ),
        rejected("pack_not_installed")
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("graphify".into(), false, 0, 1)
                .await
                .unwrap()
        ),
        rejected("pack_not_installed")
    );
    assert_eq!(
        result(
            owner
                .set_pack_entry("superpowers".into(), false, 0, 1)
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
                .set_pack_entry("bad id!".into(), true, 0, 1)
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

const CLEAN_ENV_CHILD: &str = "CRAZYAGENT_SETUP_ROW_CLEAN_ENV";
const CLEAN_ENV_WAIT: Duration = Duration::from_secs(120);
const CLEAN_ENV_POLL: Duration = Duration::from_millis(50);
const CLEAN_ENV_KEPT: [&str; 4] = [
    "TMPDIR",
    "USER",
    "LOGNAME",
    "CRAZYAGENT_TEST_NATIVE_AUTH_HOST",
];

/// Runs the named row again in a child process with a private HOME and only the system
/// PATH, as the engine finds the PATH's and HOME's tools ahead of its own setup folder, so a
/// graphify the owner or the farm installed never answers for a fixture. Input: the row's
/// name. Output: true in the child, which runs the row; false in the parent once the child
/// passed. Panics with the child's output when it fails or outlives `CLEAN_ENV_WAIT`.
fn in_clean_env(row: &str) -> bool {
    if std::env::var_os(CLEAN_ENV_CHILD).is_some() {
        return true;
    }
    let home = tempfile::Builder::new()
        .prefix("ca-setup-home-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let log = home.path().join("child.log");
    let out = fs::File::create(&log).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([row, "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CLEAN_ENV_CHILD, "1")
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(std::process::Stdio::null())
        .stdout(out.try_clone().unwrap())
        .stderr(out);
    for name in CLEAN_ENV_KEPT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
    let deadline = Instant::now() + CLEAN_ENV_WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(CLEAN_ENV_POLL);
    };
    let output = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.is_some_and(|status| status.success()),
        "{row} in a clean HOME and PATH: {status:?}\n{output}"
    );
    false
}

/// Wiring test: a fake graphify that sleeps and prints a version no real install has stands
/// in for the real tool, found in the engine's setup folder by a child with no other
/// graphify on its HOME or PATH ([`in_clean_env`]), so this checks the status read deadline
/// only. setup_status_lists_the_dependencies_and_the_shipped_pack accepts Found or Missing.
#[tokio::test]
async fn wiring_a_version_check_slower_than_the_reply_deadline_does_not_fail_the_status_read() {
    if !in_clean_env(
        "wiring_a_version_check_slower_than_the_reply_deadline_does_not_fail_the_status_read",
    ) {
        return;
    }
    let case = Case::new().await;
    let setup = case.root.path().join("setup");
    let bin = setup.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o700)).unwrap();
    let program = bin.join("graphify");
    fs::write(&program, "#!/bin/sh\n/bin/sleep 4\necho graphify 9.8.7\n").unwrap();
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
            version: Version::parse("9.8.7").unwrap()
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
