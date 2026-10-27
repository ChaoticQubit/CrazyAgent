#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Starts a real engine in a private scratch root. Output: the root, the running service
/// and the stop sender that keeps it running.
async fn engine() -> (
    tempfile::TempDir,
    EngineService,
    tokio::sync::watch::Sender<bool>,
) {
    let root = tempfile::Builder::new()
        .prefix("ca-files-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    fs::write(&global, "timeout_ms = 5000\n").unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    (root, service, stop)
}

/// Runs git as a test owner would, with an empty home and no system config.
/// Inputs: the home, the folder and the arguments.
fn git(home: &Path, dir: &Path, args: &[&str]) {
    let status = crazyagent::spawn_gate::status(
        std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1"),
    )
    .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Sets up one project through the real engine and returns its folder.
/// Inputs: the owner controller, the projects home and the project id.
async fn set_up(owner: &mut OwnerController, home: &Path, project: &ProjectId) -> PathBuf {
    owner.read(OwnerRead::ProjectsHome).await.unwrap();
    owner
        .begin_mutation(
            OwnerMutation::SetProjectsHome {
                path: home.to_str().unwrap().into(),
            },
            1,
        )
        .unwrap();
    match owner.send_pending().await.unwrap() {
        OwnerResponse::Applied { operation } => assert_eq!(operation.result, OwnerResult::Applied),
        other => panic!("{other:?}"),
    }
    let setup = ProjectSetup {
        about: "Sells plants.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: "Plant shop".into(),
        repositories: vec![],
    };
    match owner
        .setup_project(project.clone(), spec, setup, 2)
        .await
        .unwrap()
    {
        OwnerResponse::Applied { operation } => assert_eq!(operation.result, OwnerResult::Applied),
        other => panic!("{other:?}"),
    }
    match owner
        .read(OwnerRead::ProjectSetup {
            project_id: project.clone(),
        })
        .await
        .unwrap()
        .data
    {
        OwnerData::ProjectSetup {
            setup: Some(setup), ..
        } => setup.project_folder(),
        other => panic!("{other:?}"),
    }
}

fn sorted(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_lists_a_projects_tracked_and_untracked_files_and_refuses_an_unknown_project() {
    let (root, mut service, _stop) = engine().await;
    let scratch = tempfile::tempdir().unwrap();
    let place = scratch.path().canonicalize().unwrap();
    let (home, git_home, outside) = (
        place.join("projects"),
        place.join("git-home"),
        place.join("outside"),
    );
    for folder in [&home, &git_home, &outside] {
        fs::create_dir(folder).unwrap();
    }
    fs::write(outside.join("secret.txt"), "secret").unwrap();
    let mut owner = OwnerController::attach(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let project = ProjectId::new("plant-shop").unwrap();
    let folder = set_up(&mut owner, &home, &project).await;
    assert!(
        folder.join(".git").is_dir(),
        "the engine makes the project repository"
    );
    fs::create_dir_all(folder.join("src")).unwrap();
    fs::write(folder.join("src/cart.rs"), "pub fn cart() {}").unwrap();
    fs::write(folder.join(".gitignore"), "build/\n").unwrap();
    git(&git_home, &folder, &["add", "src/cart.rs"]);
    fs::write(folder.join("TODO.txt"), "water the ferns").unwrap();
    fs::create_dir(folder.join("build")).unwrap();
    fs::write(folder.join("build/app.bin"), "ignored").unwrap();
    symlink(&outside, folder.join("outside-link")).unwrap();
    symlink(outside.join("secret.txt"), folder.join("secret.txt")).unwrap();
    let before = sorted(
        owner
            .project_files(project.clone(), String::new(), 200)
            .await
            .unwrap(),
    );
    assert!(before.contains(&"src/cart.rs".to_owned()), "{before:?}");
    assert!(before.contains(&"TODO.txt".to_owned()), "{before:?}");
    assert!(before.contains(&".gitignore".to_owned()), "{before:?}");
    for absent in ["build/app.bin", "outside-link", "secret.txt"] {
        assert!(
            !before.iter().any(|path| path == absent),
            "{absent} listed: {before:?}"
        );
    }
    assert!(
        before
            .iter()
            .all(|path| !path.starts_with(".git/") && !path.starts_with('/')),
        "{before:?}"
    );
    assert_eq!(
        owner
            .project_files(project.clone(), "CART".into(), 200)
            .await
            .unwrap(),
        ["src/cart.rs"]
    );
    assert_eq!(
        owner
            .project_files(project.clone(), String::new(), 1)
            .await
            .unwrap()
            .len(),
        1
    );
    let refused =
        |result: Result<Vec<String>, crazyagent::engine::EngineError>| result.unwrap_err().0;
    assert_eq!(
        refused(
            owner
                .project_files(ProjectId::new("absent").unwrap(), String::new(), 200)
                .await
        ),
        "project_setup_missing"
    );
    assert_eq!(
        refused(owner.project_files(project.clone(), String::new(), 0).await),
        "project_files_limit_invalid"
    );
    assert_eq!(
        refused(
            owner
                .project_files(project.clone(), "a\u{7}".into(), 200)
                .await
        ),
        "project_files_query_invalid"
    );
    service.shutdown().await.unwrap();
}
