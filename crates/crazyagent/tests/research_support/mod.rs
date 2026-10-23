#![allow(dead_code)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::project_setup::ProjectSetup;
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

pub const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";

/// A real engine runtime folder: `engine.db` holding projects created through
/// the owner client of a real engine, one engine-made folder per project in
/// `projects/`, and a private `research/` state folder for the runner. The
/// engine is stopped once the projects exist, so only the runner uses it.
pub struct EngineRuntime {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub database: PathBuf,
    pub state: PathBuf,
    pub global: PathBuf,
    pub project_settings: PathBuf,
}

impl EngineRuntime {
    /// Creates each `(project id, about)` pair as a real project with no
    /// research toggles, so tests queue exactly the jobs they need.
    pub fn with_projects(projects: &[(&str, &str)]) -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ca-research-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let global = root.join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let project_settings = root.join("project.toml");
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(create_projects(&root, &global, &project_settings, projects));
        let state = root.join("research");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _temp: temp,
            database: root.join("engine.db"),
            state,
            root,
            global,
            project_settings,
        }
    }

    /// The canonical folder the engine made for `project`.
    pub fn folder(&self, project: &str) -> PathBuf {
        let folder = self
            .root
            .join("projects")
            .join(project)
            .canonicalize()
            .unwrap();
        assert!(folder.join("context").join("brief.md").is_file());
        folder
    }

    /// A fresh 0700 directory under the runtime that stands in for an
    /// engine-owned managed profile home nobody has signed in to.
    pub fn private_home(&self, name: &str) -> PathBuf {
        let home = self.root.join(name);
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        home.canonicalize().unwrap()
    }
}

/// Creates projects through the owner client of a running engine.
pub async fn create_projects(
    root: &Path,
    global: &Path,
    project_settings: &Path,
    projects: &[(&str, &str)],
) {
    let (stop, stopped) = watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root,
        "e.sock",
        global,
        project_settings,
        &stopped,
    )
    .await
    .unwrap();
    let mut owner = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();
    setup_projects(&mut owner, projects).await;
    drop(owner);
    service.shutdown().await.unwrap();
    drop(stop);
}

/// Creates each `(project id, about)` pair through an attached owner client.
pub async fn setup_projects(owner: &mut OwnerController, projects: &[(&str, &str)]) {
    for (generation, (id, about)) in (1_u64..).zip(projects) {
        let response = owner
            .setup_project(
                ProjectId::new(*id).unwrap(),
                ProjectSpec {
                    name: (*id).into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: (*about).into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                generation,
            )
            .await
            .unwrap();
        match response {
            OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
                assert_eq!(operation.result, OwnerResult::Applied)
            }
            other => panic!("project creation must commit: {other:?}"),
        }
    }
}

/// The real Codex executable, from `CRAZYAGENT_CODEX` or Homebrew.
pub fn codex() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/codex"))
        .canonicalize()
        .expect("real codex binary required");
    assert!(
        path.is_file(),
        "real codex binary required: {}",
        path.display()
    );
    path
}

/// The real Claude Code executable, from `CRAZYAGENT_CLAUDE` or Homebrew.
pub fn claude() -> PathBuf {
    std::env::var_os("CRAZYAGENT_CLAUDE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/claude"))
        .canonicalize()
        .expect("real claude binary required")
}

/// The signed-in managed profile home named by `variable`, or a panic with
/// the plain BLOCKED message and the command the owner runs to unblock it.
pub fn managed_home(variable: &str, harness: &str) -> PathBuf {
    let Some(home) = std::env::var_os(variable) else {
        panic!(
            "BLOCKED: no signed-in managed {harness} account. Sign in to one in CrazyAgent, then run: {variable}=<its managed profile home> cargo test --locked --offline -p crazyagent --test research_runner -- --test-threads=1"
        );
    };
    let home = PathBuf::from(home)
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: {variable} does not name an existing folder"));
    assert!(home.is_dir(), "BLOCKED: {variable} must name a folder");
    home
}
