#![allow(dead_code)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::project_setup::ProjectSetup;
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::sync::watch;

pub const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";

/// A real engine runtime folder: `engine.db` holding projects created through
/// the owner client of a real engine, one engine-made folder per project in
/// a separate projects home, and a private `research/` state folder for the
/// runner. The engine is stopped once the projects exist, so only the runner
/// uses it.
pub struct EngineRuntime {
    _temp: tempfile::TempDir,
    _home: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
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
        let home_temp = tempfile::Builder::new()
            .prefix("ca-home-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let home = home_temp.path().canonicalize().unwrap();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(create_projects(
                &root,
                &global,
                &project_settings,
                &home,
                projects,
            ));
        let state = root.join("research");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _temp: temp,
            _home: home_temp,
            database: root.join("engine.db"),
            state,
            root,
            home,
            global,
            project_settings,
        }
    }

    /// The canonical folder the engine made for `project` in the home.
    pub fn folder(&self, project: &str) -> PathBuf {
        let name = crazyagent::projects_home::slug(project).unwrap();
        let folder = self.home.join(name).canonicalize().unwrap();
        assert!(crazyagent_in(&folder).join("context/brief.md").is_file());
        folder
    }

    /// The `.crazyagent` folder holding `project`'s context and worktrees.
    pub fn crazyagent(&self, project: &str) -> PathBuf {
        crazyagent_in(&self.folder(project))
    }
}

/// The `.crazyagent` folder of an engine-made project folder.
fn crazyagent_in(folder: &Path) -> PathBuf {
    let inner = crazyagent::projects_home::state_folder(folder);
    assert_eq!(inner, folder.join(".crazyagent"));
    inner
}

/// Creates projects in `home` through the owner client of a running engine.
pub async fn create_projects(
    root: &Path,
    global: &Path,
    project_settings: &Path,
    home: &Path,
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
    setup_projects(&mut owner, home, projects).await;
    drop(owner);
    service.shutdown().await.unwrap();
    drop(stop);
}

/// Sets `home` as the projects home, then creates each `(project id, about)`
/// pair in it through an attached owner client.
pub async fn setup_projects(owner: &mut OwnerController, home: &Path, projects: &[(&str, &str)]) {
    let response = owner
        .set_projects_home(home.to_str().unwrap().into(), 1)
        .await
        .unwrap();
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("setting the projects home must commit: {other:?}"),
    }
    for (generation, (id, about)) in (2_u64..).zip(projects) {
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

/// The runtime folder of a running engine where the owner signed in a
/// managed account, and the project named by `variable` whose assigned pool
/// holds it. Panics with the plain BLOCKED message and the steps the owner
/// takes to unblock the paid run when either is not set.
pub fn managed_project(variable: &str, harness: &str) -> (PathBuf, String) {
    let blocked = || -> ! {
        panic!(
            "BLOCKED: no signed-in managed {harness} account. Start CrazyAgent's engine with --writable-settings, --codex-executable and --claude-executable, sign in to a managed {harness} account, assign its pool to a project, then run: CRAZYAGENT_MANAGED_RUNTIME=<engine runtime folder> {variable}=<that project id> cargo test --locked --offline -p crazyagent --test research_managed"
        )
    };
    let runtime = std::env::var_os("CRAZYAGENT_MANAGED_RUNTIME").unwrap_or_else(|| blocked());
    let project = std::env::var(variable).unwrap_or_else(|_| blocked());
    let runtime = PathBuf::from(runtime)
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: CRAZYAGENT_MANAGED_RUNTIME does not name a folder"));
    assert!(
        runtime.join("engine.db").is_file(),
        "BLOCKED: CRAZYAGENT_MANAGED_RUNTIME has no engine.db"
    );
    (runtime, project)
}

/// A real local model endpoint that accepts up to 64 connections, reads each
/// request once and never answers, so an agent pointed at it keeps running
/// until it is stopped. Returns the port and the serving thread.
pub fn hanging_endpoint() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().take(64) {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            held.push(stream);
        }
    });
    (port, handle)
}

/// The ids of the live processes whose command line contains `marker`, such
/// as a research agent whose `-o` report path sits in `<state>/agents/`.
pub fn processes_with(marker: &Path) -> Vec<u32> {
    let output = Command::new("/usr/bin/pgrep")
        .env_clear()
        .arg("-f")
        .arg(marker)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect()
}
