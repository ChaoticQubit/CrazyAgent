#![cfg(unix)]

mod research_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerMutation, OwnerResponse, OwnerResult};
use crazyagent::onboarding::service::{
    CODEX_MISSING, OnboardingLimits, OnboardingService, installed_graphify,
};
use crazyagent::onboarding::{OnboardingView, Step, project_id_for};
use crazyagent::project_setup::Platform;
use crazyagent::projects_home::{FoundProject, FoundState};
use crazyagent::protocol::{CredentialRef, ProjectId};
use crazyagent::research::accounts::SettingsResolver;
use crazyagent::research::runner::{
    ResearchAccount, ResearchHarness, ResearchLimits, ResearchRunner,
};
use research_support::{claude, codex, hanging_endpoint, processes_with};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POOL: &str = "local-pool";
const PROJECT: &str = "demo-app";
const GRAPHIFY_MISSING: &str = "graphify not installed";
const NO_ACCOUNT: &str = "No signed-in account in this pool can map the project";
const MODEL: &str = "qwen2.5:7b";
const OLLAMA: &str = "127.0.0.1:11434";
const OLLAMA_TIME: Duration = Duration::from_secs(10);
const OLLAMA_TAGS_BYTES_MAX: u64 = 1024 * 1024;
const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:7b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"demo-app\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";
const SAVED_DRAFT: &str = r#"{"name":"Demo Tool","draft":{"about":"Kept by the owner.","tech_stack":["Postgres"],"platforms":["cli"],"research":[],"pool":null,"team":null,"documents":[]},"facts":{"languages":[],"build_commands":[],"test_commands":[],"entry_points":[]}}"#;
const OWNER_OVERVIEW: &str = "# Owner overview\n\nWritten by hand.\n";

/// Fails the test with the command that fixes it unless the local Ollama serves the pool
/// model. Input: none; reads the model list from Ollama's `/api/tags`. Output: none.
fn require_model() {
    assert!(CATALOG.contains(&format!("model = {{ id = \"{MODEL}\"")));
    let listed = crazyagent::spawn_gate::connect_tcp_std(OLLAMA).and_then(|mut stream| {
        stream.set_read_timeout(Some(OLLAMA_TIME))?;
        stream.set_write_timeout(Some(OLLAMA_TIME))?;
        stream.write_all(
            format!("GET /api/tags HTTP/1.1\r\nHost: {OLLAMA}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )?;
        let mut reply = String::new();
        stream
            .take(OLLAMA_TAGS_BYTES_MAX)
            .read_to_string(&mut reply)?;
        Ok(reply)
    });
    let reply = listed.unwrap_or_else(|problem| {
        panic!("the local Ollama at {OLLAMA} is required and did not answer: {problem}")
    });
    assert!(
        reply.contains(&format!("\"name\":\"{MODEL}\"")),
        "the local Ollama does not have {MODEL}; run: ollama pull {MODEL}"
    );
}

/// Runs git in `repo` with no global or system configuration.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/git")
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Onboarding Test",
                "-c",
                "user.email=onboarding@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Makes `repo` a real git repository of a small Rust command line tool.
fn rust_repo(repo: &Path) {
    fs::create_dir(repo).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"demo-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("README.md"),
        "# demo-app\n\nA small command line tool that prints a greeting.\n",
    )
    .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "first"]);
}

fn settled(step: &Step) -> bool {
    matches!(step, Step::Done | Step::Failed { .. })
}

fn failed(reason: &str) -> Step {
    Step::Failed {
        reason: reason.into(),
    }
}

/// The plain code of a rejected owner response, or `None` once applied.
fn rejection(response: &OwnerResponse) -> Option<String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match &operation.result {
                OwnerResult::Applied => None,
                OwnerResult::Rejected { code } => Some(code.clone()),
                other => panic!("unexpected owner result: {other:?}"),
            }
        }
        OwnerResponse::Rejected { code } => Some(code.clone()),
        other => panic!("unexpected owner response: {other:?}"),
    }
}

/// The real `crazyagent engine` binary on a runtime folder, started with
/// writable settings and no `--codex-executable` or `--claude-executable`.
struct EngineProcess {
    child: Child,
}

impl EngineProcess {
    /// Starts the binary and waits up to 20 s for its `ready` line.
    fn start(root: &Path, global: &Path, project_settings: &Path) -> Self {
        let mut child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(root)
                .arg(global)
                .arg(project_settings)
                .arg("engine.sock")
                .arg("--writable-settings")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, ready) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = send.send(line);
            let mut rest = Vec::new();
            let _ = reader.take(65536).read_to_end(&mut rest);
        });
        let line = ready
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_default();
        let engine = Self { child };
        assert_eq!(line, "ready\n", "engine without codex did not start");
        engine
    }

    /// Sends SIGTERM, as the owner's stop does, and waits up to 30 s for a
    /// clean exit. Returns whether the engine exited successfully.
    fn terminate(&mut self) -> bool {
        if let Some(status) = self.child.try_wait().ok().flatten() {
            return status.success();
        }
        let _ = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &self.child.id().to_string(),
        ]));
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().ok().flatten() {
                return status.success();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        false
    }
}

impl Drop for EngineProcess {
    /// Stops an engine a failed test left running, so no step process
    /// outlives the test.
    fn drop(&mut self) {
        self.terminate();
    }
}

/// The engine a bench drives: in this test process, or the real binary.
enum Engine {
    InProcess(Box<EngineService>, watch::Sender<bool>),
    Binary(EngineProcess),
}

/// A real engine with the local pool in its settings, its owner client and a
/// real git repository outside the engine's runtime folder.
struct Bench {
    _runtime: tempfile::TempDir,
    _repos: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    global: PathBuf,
    project_settings: PathBuf,
    engine: Engine,
    owner: OwnerController,
    generation: u64,
}

/// The private runtime folder with the settings, and the committed repo
/// beside it. `prepare` may add files to the repo.
struct Folders {
    runtime: tempfile::TempDir,
    repos: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    global: PathBuf,
    project_settings: PathBuf,
}

impl Folders {
    fn new(prepare: impl FnOnce(&Path)) -> Self {
        let runtime = tempfile::Builder::new()
            .prefix("ca-onboard-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = runtime.path().canonicalize().unwrap();
        let global = root.join("global.toml");
        fs::write(&global, CATALOG).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let project_settings = root.join("project.toml");
        fs::write(&project_settings, "").unwrap();
        fs::set_permissions(&project_settings, fs::Permissions::from_mode(0o600)).unwrap();
        let repos = tempfile::Builder::new()
            .prefix("ca-onboard-repos-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let repo = repos.path().canonicalize().unwrap().join(PROJECT);
        rust_repo(&repo);
        prepare(&repo);
        Self {
            runtime,
            repos,
            root,
            repo,
            global,
            project_settings,
        }
    }
}

impl Bench {
    fn with(folders: Folders, engine: Engine, owner: OwnerController) -> Self {
        Self {
            _runtime: folders.runtime,
            _repos: folders.repos,
            root: folders.root,
            repo: folders.repo,
            global: folders.global,
            project_settings: folders.project_settings,
            engine,
            owner,
            generation: 0,
        }
    }

    /// Starts an in-process engine; `prepare` may add files to the repo.
    async fn start(prepare: impl FnOnce(&Path)) -> Self {
        let folders = Folders::new(prepare);
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            &folders.root,
            "e.sock",
            &folders.global,
            &folders.project_settings,
            &stopped,
        )
        .await
        .unwrap();
        let owner =
            OwnerController::attach(&folders.root, OwnerClientKind::Tui, Duration::from_secs(5))
                .await
                .unwrap();
        Self::with(folders, Engine::InProcess(Box::new(service), stop), owner)
    }

    /// Starts the real engine binary without Codex or Claude Code and
    /// attaches the owner client, retrying for up to 20 s.
    async fn start_binary() -> Self {
        let folders = Folders::new(|_| {});
        let process =
            EngineProcess::start(&folders.root, &folders.global, &folders.project_settings);
        let deadline = Instant::now() + Duration::from_secs(20);
        let owner = loop {
            match OwnerController::attach(
                &folders.root,
                OwnerClientKind::Tui,
                Duration::from_secs(2),
            )
            .await
            {
                Ok(owner) => break owner,
                Err(error) => {
                    assert!(Instant::now() < deadline, "owner attach: {error:?}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        Self::with(folders, Engine::Binary(process), owner)
    }

    /// Asks the engine to onboard `path` on the local pool through the owner
    /// client. Returns the owner response.
    async fn start_onboarding_at(&mut self, path: &Path) -> OwnerResponse {
        self.generation += 1;
        self.owner
            .start_onboarding(
                path.to_str().unwrap().to_owned(),
                CredentialRef::new(POOL).unwrap(),
                self.generation,
            )
            .await
            .unwrap()
    }

    /// Onboards the bench repository and returns its project id.
    async fn start_onboarding(&mut self) -> ProjectId {
        let repo = self.repo.clone();
        let response = self.start_onboarding_at(&repo).await;
        assert_eq!(rejection(&response), None, "{response:?}");
        let project = project_id_for(&self.repo).unwrap();
        assert_eq!(project.as_str(), PROJECT);
        project
    }

    /// Sets the folder holding the bench repository as the projects home.
    async fn use_repos_as_home(&mut self) {
        self.generation += 1;
        let home = self.repo.parent().unwrap().to_str().unwrap().to_owned();
        let response = self
            .owner
            .set_projects_home(home, self.generation)
            .await
            .unwrap();
        assert_eq!(rejection(&response), None, "{response:?}");
    }

    /// The projects home entry of the bench repository.
    async fn home_entry(&mut self) -> FoundProject {
        let found = self.owner.found_projects().await.unwrap();
        found
            .into_iter()
            .find(|entry| Path::new(&entry.path) == self.repo)
            .unwrap()
    }

    async fn view(&mut self, project: &ProjectId) -> OnboardingView {
        self.owner.onboarding(project.clone()).await.unwrap()
    }

    /// Reads the onboarding until both steps finished or `limit` passed.
    async fn settle(&mut self, project: &ProjectId, limit: Duration) -> OnboardingView {
        let deadline = Instant::now() + limit;
        loop {
            let view = self.view(project).await;
            if settled(&view.graph) && settled(&view.mapping) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "onboarding did not settle: {view:?}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// A private state folder in the runtime for a test's own runner.
    fn state(&self) -> PathBuf {
        let state = self.root.join("onboarding-test");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        state.canonicalize().unwrap()
    }

    /// A Codex runner with its own private state folder in the runtime, and
    /// that folder's `agents/` where every step's agent state goes.
    fn runner(&self) -> (Arc<ResearchRunner>, PathBuf) {
        let state = self.state();
        let runner = ResearchRunner::new(
            &self.root.join("engine.db"),
            &state,
            &codex(),
            ResearchLimits::default(),
        )
        .unwrap();
        (Arc::new(runner), state.join("agents"))
    }

    /// A runner with no Codex, and Claude Code only when `with_claude`.
    fn runner_without_codex(&self, with_claude: bool) -> (Arc<ResearchRunner>, PathBuf) {
        let state = self.state();
        let mut runner = ResearchRunner::without_codex(
            &self.root.join("engine.db"),
            &state,
            ResearchLimits::default(),
        )
        .unwrap();
        if with_claude {
            runner = runner.with_claude(&claude()).unwrap();
        }
        assert!(!runner.can_run(ResearchHarness::Codex));
        assert_eq!(runner.can_run(ResearchHarness::ClaudeCode), with_claude);
        (Arc::new(runner), state.join("agents"))
    }

    async fn stop(self) {
        let Self { engine, owner, .. } = self;
        drop(owner);
        match engine {
            Engine::InProcess(mut service, _stop) => service.shutdown().await.unwrap(),
            Engine::Binary(mut process) => {
                let clean = tokio::task::spawn_blocking(move || process.terminate())
                    .await
                    .unwrap();
                assert!(clean, "engine without codex did not stop cleanly");
            }
        }
    }
}

async fn stop_service(service: OnboardingService) {
    tokio::task::spawn_blocking(move || service.stop())
        .await
        .unwrap();
}

fn agent_runs(agents: &Path, prefix: &str) -> usize {
    fs::read_dir(agents)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_found_rust_repo_is_onboarded_end_to_end_with_a_local_model() {
    require_model();
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    assert_eq!(bench.home_entry().await.state, FoundState::Onboarding);
    let onboarding = crazyagent::onboarding::service::start_for_engine(
        &bench.root,
        &bench.global,
        &bench.project_settings,
        Some(&codex()),
        None,
    )
    .unwrap();
    let view = bench.settle(&project, Duration::from_secs(1200)).await;
    stop_service(onboarding).await;
    assert_eq!(
        view.graph,
        Step::Done,
        "BLOCKED when graphify is not installed: {view:?}"
    );
    assert_eq!(view.mapping, Step::Done, "{view:?}");
    assert!(bench.repo.join("graphify-out/graph.json").is_file());
    assert!(bench.repo.join(".crazyagent/overview.md").is_file());
    assert!(bench.repo.join(".crazyagent/setup/draft.json").is_file());
    assert!(bench.repo.join(".crazyagent/context/brief.md").is_file());
    assert!(
        view.overview
            .as_deref()
            .is_some_and(|overview| !overview.trim().is_empty())
    );
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert!(
        view.facts
            .build_commands
            .contains(&"cargo build".to_owned())
    );
    assert!(view.draft.research.is_empty());
    assert_eq!(
        view.draft.pool.as_ref().map(CredentialRef::as_str),
        Some(POOL)
    );
    assert_eq!(git(&bench.repo, &["status", "--porcelain"]), "");
    assert!(processes_with(&bench.root.join("onboarding")).is_empty());
    let entry = bench.home_entry().await;
    assert_eq!(entry.state, FoundState::Ready, "{entry:?}");
    assert_eq!(entry.project_id.as_ref(), Some(&project));
    let setup = bench.owner.project_setup(project).await.unwrap().unwrap();
    assert!(setup.research.is_empty());
    assert_eq!(
        Path::new(&setup.folder),
        bench.repo.join(".crazyagent").as_path()
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_graphify_fails_with_its_plain_reason_and_a_second_start_retries() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let waiting = bench.view(&project).await;
    assert_eq!(waiting.graph, Step::Waiting);
    assert_eq!(waiting.mapping, Step::Waiting);
    assert!(waiting.draft.research.is_empty());
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &CredentialRef, _: &[ResearchHarness]| None,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.graph, failed(GRAPHIFY_MISSING));
    assert_eq!(view.mapping, failed(NO_ACCOUNT));
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert!(view.missing.contains(&"about".to_owned()));
    assert!(!bench.repo.join("graphify-out").join("graph.json").exists());
    assert_eq!(agent_runs(&agents, "onboard-"), 0);
    assert_eq!(bench.home_entry().await.state, FoundState::Onboarding);
    let again = bench.start_onboarding().await;
    assert_eq!(again, project);
    let retried = bench.view(&project).await;
    assert_eq!(retried.graph, Step::Waiting);
    assert_eq!(retried.mapping, Step::Waiting);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_projects_home_shows_an_onboarding_project_as_setting_up_and_keeps_its_folder() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let entry = bench.home_entry().await;
    assert_eq!(entry.state, FoundState::Found, "{entry:?}");
    assert_eq!(entry.project_id, None);
    let project = bench.start_onboarding().await;
    let waiting = bench.view(&project).await;
    assert_eq!(
        (waiting.graph, waiting.mapping),
        (Step::Waiting, Step::Waiting)
    );
    let entry = bench.home_entry().await;
    assert_eq!(entry.state, FoundState::Onboarding, "{entry:?}");
    assert_eq!(entry.project_id.as_ref(), Some(&project));
    assert!(entry.git && entry.known);
    let found = bench.owner.found_projects().await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    bench.generation += 1;
    bench
        .owner
        .begin_mutation(
            OwnerMutation::DeleteProjectFolder {
                path: bench.repo.to_str().unwrap().to_owned(),
            },
            bench.generation,
        )
        .unwrap();
    let response = bench.owner.send_pending().await.unwrap();
    assert_eq!(
        rejection(&response).as_deref(),
        Some("home_trash_still_set_up")
    );
    assert!(bench.repo.join(".crazyagent").is_dir());
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mapping_agent_that_runs_out_of_time_leaves_no_process() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &CredentialRef, _: &[ResearchHarness]| Some(account.clone()),
        || None,
        OnboardingLimits {
            graph_time: Duration::from_secs(60),
            mapping_time: Duration::from_secs(8),
        },
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(120)).await;
    assert_eq!(view.mapping, failed("Mapping agent ran out of time"));
    assert_eq!(agent_runs(&agents, "onboard-m-"), 1);
    assert!(processes_with(&agents).is_empty());
    stop_service(service).await;
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_service_mid_mapping_stops_the_agent_and_requeues_the_step() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &CredentialRef, _: &[ResearchHarness]| Some(account.clone()),
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let view = bench.view(&project).await;
        if view.mapping == Step::Running && !processes_with(&agents).is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "mapping agent never started: {view:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop_service(service).await;
    assert!(processes_with(&agents).is_empty());
    let view = bench.view(&project).await;
    assert_eq!(view.graph, failed(GRAPHIFY_MISSING));
    assert_eq!(view.mapping, Step::Waiting);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_crazyagent_folder_is_reused_not_overwritten() {
    let mut bench = Bench::start(|repo| {
        fs::create_dir_all(repo.join(".crazyagent/setup")).unwrap();
        fs::write(repo.join(".crazyagent/setup/draft.json"), SAVED_DRAFT).unwrap();
        fs::write(repo.join(".crazyagent/overview.md"), OWNER_OVERVIEW).unwrap();
    })
    .await;
    let project = bench.start_onboarding().await;
    let reused = bench.view(&project).await;
    assert_eq!(reused.name, "Demo Tool");
    assert_eq!(reused.draft.about, "Kept by the owner.");
    assert_eq!(reused.draft.platforms, [Platform::Cli]);
    assert_eq!(
        reused.draft.pool.as_ref().map(CredentialRef::as_str),
        Some(POOL)
    );
    assert!(reused.draft.research.is_empty());
    assert_eq!(reused.overview.as_deref(), Some(OWNER_OVERVIEW.trim()));
    let (runner, _) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &CredentialRef, _: &[ResearchHarness]| None,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.draft.about, "Kept by the owner.");
    assert_eq!(view.draft.tech_stack, ["Postgres", "Rust"]);
    assert_eq!(
        fs::read_to_string(bench.repo.join(".crazyagent/overview.md")).unwrap(),
        OWNER_OVERVIEW
    );
    assert!(bench.repo.join(".crazyagent/context/brief.md").is_file());
    let setup = bench.owner.project_setup(project).await.unwrap().unwrap();
    assert_eq!(setup.name, "Demo Tool");
    assert_eq!(setup.about, "Kept by the owner.");
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsafe_or_unknown_folders_are_refused_with_plain_codes() {
    let mut bench = Bench::start(|_| {}).await;
    let inside = bench.root.join("inside");
    fs::create_dir(&inside).unwrap();
    let response = bench.start_onboarding_at(&inside).await;
    assert_eq!(
        rejection(&response).as_deref(),
        Some("onboarding_folder_unsafe")
    );
    let link = bench.repo.with_file_name("linked-app");
    std::os::unix::fs::symlink(&bench.repo, &link).unwrap();
    let response = bench.start_onboarding_at(&link).await;
    assert_eq!(
        rejection(&response).as_deref(),
        Some("onboarding_folder_missing")
    );
    let absent = bench.repo.with_file_name("absent-app");
    let response = bench.start_onboarding_at(&absent).await;
    assert_eq!(
        rejection(&response).as_deref(),
        Some("onboarding_folder_missing")
    );
    assert!(!bench.repo.join(".crazyagent").exists());
    bench.stop().await;
}

/// The installed graphify, or a BLOCKED panic naming the missing tool.
fn graphify() -> PathBuf {
    installed_graphify().expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_started_without_codex_builds_the_graph_and_reports_codex_missing() {
    graphify();
    let mut bench = Bench::start_binary().await;
    let project = bench.start_onboarding().await;
    let view = bench.settle(&project, Duration::from_secs(660)).await;
    assert_eq!(view.graph, Step::Done, "{view:?}");
    assert_eq!(view.mapping, failed(CODEX_MISSING), "{view:?}");
    assert!(bench.repo.join("graphify-out/graph.json").is_file());
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert!(
        view.facts
            .build_commands
            .contains(&"cargo build".to_owned())
    );
    assert!(view.overview.is_none());
    assert!(bench.repo.join(".crazyagent/setup/draft.json").is_file());
    assert_eq!(git(&bench.repo, &["status", "--porcelain"]), "");
    let onboarding_state = bench.root.join("onboarding");
    assert!(onboarding_state.is_dir());
    bench.stop().await;
    assert!(processes_with(&onboarding_state).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_codex_or_claude_mapping_fails_before_any_account_is_picked() {
    let graphify = graphify();
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
    };
    let asked = Arc::new(Mutex::new(0_usize));
    let (runner, agents) = bench.runner_without_codex(false);
    let service = {
        let asked = asked.clone();
        OnboardingService::start(
            runner,
            move |_: &CredentialRef, _: &[ResearchHarness]| {
                *asked.lock().unwrap() += 1;
                Some(account.clone())
            },
            move || Some(graphify.clone()),
            OnboardingLimits::default(),
            Duration::from_millis(50),
        )
    };
    let view = bench.settle(&project, Duration::from_secs(660)).await;
    stop_service(service).await;
    assert_eq!(view.graph, Step::Done, "{view:?}");
    assert_eq!(view.mapping, failed(CODEX_MISSING), "{view:?}");
    assert_eq!(*asked.lock().unwrap(), 0);
    assert_eq!(agent_runs(&agents, "onboard-g-"), 1);
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_only_claude_code_installed_mapping_asks_for_a_claude_code_account() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let seen: Arc<Mutex<Vec<Vec<ResearchHarness>>>> = Arc::new(Mutex::new(Vec::new()));
    let (runner, agents) = bench.runner_without_codex(true);
    let service = {
        let seen = seen.clone();
        OnboardingService::start(
            runner,
            move |_: &CredentialRef, harnesses: &[ResearchHarness]| {
                seen.lock().unwrap().push(harnesses.to_vec());
                Some(ResearchAccount::Endpoint {
                    base_url: "http://127.0.0.1:9/v1".into(),
                    model: "codex-only-model".into(),
                })
            },
            || None,
            OnboardingLimits::default(),
            Duration::from_millis(50),
        )
    };
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.graph, failed(GRAPHIFY_MISSING));
    assert_eq!(view.mapping, failed(NO_ACCOUNT), "{view:?}");
    assert_eq!(*seen.lock().unwrap(), [vec![ResearchHarness::ClaudeCode]]);
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pool_pick_offers_only_accounts_of_installed_harnesses() {
    let bench = Bench::start(|_| {}).await;
    let projects = bench.root.join("projects");
    fs::create_dir_all(&projects).unwrap();
    let resolver = SettingsResolver::new(
        &bench.global,
        &bench.project_settings,
        &projects,
        &bench.root.join("engine.db"),
    )
    .unwrap();
    let pool = CredentialRef::new(POOL).unwrap();
    let local = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: MODEL.into(),
    };
    assert_eq!(
        resolver.pool_pick(&pool, &[ResearchHarness::Codex]),
        Some(local.clone())
    );
    assert_eq!(
        resolver.pool_pick(
            &pool,
            &[ResearchHarness::ClaudeCode, ResearchHarness::Codex]
        ),
        Some(local)
    );
    assert_eq!(
        resolver.pool_pick(&pool, &[ResearchHarness::ClaudeCode]),
        None
    );
    assert_eq!(resolver.pool_pick(&pool, &[]), None);
    assert_eq!(
        resolver.pool_pick(
            &CredentialRef::new("absent-pool").unwrap(),
            &[ResearchHarness::Codex]
        ),
        None
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_onboarded_folder_in_the_home_shows_as_onboarding_until_both_steps_are_done() {
    let mut bench = Bench::start(|_| {}).await;
    let home = bench.repo.parent().unwrap().to_str().unwrap().to_owned();
    bench.generation += 1;
    let set = bench
        .owner
        .set_projects_home(home, bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&set), None, "{set:?}");
    let listed = |found: &[FoundProject]| {
        found
            .iter()
            .filter(|entry| entry.name == PROJECT)
            .map(|entry| (entry.state, entry.project_id.clone(), entry.known))
            .collect::<Vec<_>>()
    };
    let before = bench.owner.found_projects().await.unwrap();
    assert_eq!(listed(&before), vec![(FoundState::Found, None, false)]);

    let project = bench.start_onboarding().await;
    let onboarding = bench.owner.found_projects().await.unwrap();
    assert_eq!(
        listed(&onboarding),
        vec![(FoundState::Onboarding, Some(project.clone()), true)]
    );
    assert!(
        onboarding
            .iter()
            .all(|entry| entry.state != FoundState::Missing),
        "{onboarding:?}"
    );

    let (runner, _) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &CredentialRef, _: &[ResearchHarness]| None,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.mapping, failed(NO_ACCOUNT), "{view:?}");
    let settled = bench.owner.found_projects().await.unwrap();
    assert_eq!(
        listed(&settled),
        vec![(FoundState::Onboarding, Some(project), true)],
        "a failed step keeps the project setting up; Ready needs both steps done"
    );
    bench.stop().await;
}

/// Counts the rows `table` still holds for the bench project in the engine database.
/// Takes the bench root and the table. Returns the row count.
fn project_rows(root: &Path, table: &str) -> i64 {
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    database
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_project_ends_its_rows_and_re_adding_the_folder_gives_a_working_project() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let database = rusqlite::Connection::open(bench.root.join("engine.db")).unwrap();
    database
        .execute_batch(&format!(
            "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES ('{PROJECT}','owner','Old question','answered',1);
             INSERT INTO team_tasks(project,source,harness,tools,prompt,status,delivered,created_ms) VALUES ('{PROJECT}','owner','codex',0,'Old step','done',1,1);
             INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms) VALUES ('{PROJECT}','market_research','codex',0,'failed',1);"
        ))
        .unwrap();
    drop(database);
    for table in ["onboarding", "team_messages", "team_tasks", "research_jobs"] {
        assert_eq!(project_rows(&bench.root, table), 1, "{table}");
    }

    bench.generation += 1;
    let response = bench
        .owner
        .remove_project(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&response), None, "{response:?}");
    for table in ["onboarding", "team_messages", "team_tasks", "research_jobs"] {
        assert_eq!(project_rows(&bench.root, table), 0, "{table} after removal");
    }
    assert!(
        bench.owner.onboarding(project.clone()).await.is_err(),
        "a removed project's waiting steps are gone, so no worker can claim them"
    );

    let again = bench.start_onboarding().await;
    assert_eq!(again, project);
    let setup = bench.owner.project_setup(project.clone()).await.unwrap();
    assert_eq!(
        setup.map(|setup| setup.project_id),
        Some(project.clone()),
        "re-adding the folder recreates the project"
    );
    let view = bench.view(&project).await;
    assert_eq!(view.project_id, project);
    let team = bench.owner.team_status(project.clone()).await.unwrap();
    assert!(team.messages.is_empty(), "{:?}", team.messages);
    assert!(team.tasks.is_empty(), "{:?}", team.tasks);
    assert!(
        bench
            .owner
            .research_jobs(project.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(project_rows(&bench.root, "onboarding"), 1);
    bench.stop().await;
}
