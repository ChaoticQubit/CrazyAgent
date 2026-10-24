mod research_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::onboarding::service::{OnboardingLimits, OnboardingService};
use crazyagent::onboarding::{OnboardingView, Step, project_id_for};
use crazyagent::project_setup::Platform;
use crazyagent::projects_home::{FoundProject, FoundState};
use crazyagent::protocol::{CredentialRef, ProjectId};
use crazyagent::research::runner::{ResearchAccount, ResearchLimits, ResearchRunner};
use research_support::{codex, hanging_endpoint, processes_with};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POOL: &str = "local-pool";
const PROJECT: &str = "demo-app";
const GRAPHIFY_MISSING: &str = "graphify not installed";
const NO_ACCOUNT: &str = "No signed-in account in this pool can map the project";
const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"demo-app\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";
const SAVED_DRAFT: &str = r#"{"name":"Demo Tool","draft":{"about":"Kept by the owner.","tech_stack":["Postgres"],"platforms":["cli"],"research":[],"pool":null,"team":null,"documents":[]},"facts":{"languages":[],"build_commands":[],"test_commands":[],"entry_points":[]}}"#;
const OWNER_OVERVIEW: &str = "# Owner overview\n\nWritten by hand.\n";

/// Runs git in `repo` with no global or system configuration.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
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
        .output()
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

/// A real in-process engine with the local pool in its settings, its owner
/// client and a real git repository outside the engine's runtime folder.
struct Bench {
    _runtime: tempfile::TempDir,
    _repos: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    global: PathBuf,
    project_settings: PathBuf,
    service: EngineService,
    owner: OwnerController,
    generation: u64,
    _stop: watch::Sender<bool>,
}

impl Bench {
    /// Starts the engine; `prepare` may add files to the committed repo.
    async fn start(prepare: impl FnOnce(&Path)) -> Self {
        let runtime = tempfile::Builder::new()
            .prefix("ca-onboard-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = runtime.path().canonicalize().unwrap();
        let global = root.join("global.toml");
        fs::write(&global, CATALOG).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let project_settings = root.join("project.toml");
        let repos = tempfile::Builder::new()
            .prefix("ca-onboard-repos-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let repo = repos.path().canonicalize().unwrap().join(PROJECT);
        rust_repo(&repo);
        prepare(&repo);
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            &root,
            "e.sock",
            &global,
            &project_settings,
            &stopped,
        )
        .await
        .unwrap();
        let owner = OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(5))
            .await
            .unwrap();
        Self {
            _runtime: runtime,
            _repos: repos,
            root,
            repo,
            global,
            project_settings,
            service,
            owner,
            generation: 0,
            _stop: stop,
        }
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

    /// A runner with its own private state folder in the runtime, and that
    /// folder's `agents/` where every step's agent state goes.
    fn runner(&self) -> (Arc<ResearchRunner>, PathBuf) {
        let state = self.root.join("onboarding-test");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let state = state.canonicalize().unwrap();
        let runner = ResearchRunner::new(
            &self.root.join("engine.db"),
            &state,
            &codex(),
            ResearchLimits::default(),
        )
        .unwrap();
        (Arc::new(runner), state.join("agents"))
    }

    async fn stop(self) {
        let Self {
            mut service, owner, ..
        } = self;
        drop(owner);
        service.shutdown().await.unwrap();
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
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let onboarding = crazyagent::onboarding::service::start_for_engine(
        &bench.root,
        &bench.global,
        &bench.project_settings,
        &codex(),
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
    let project = bench.start_onboarding().await;
    let waiting = bench.view(&project).await;
    assert_eq!(waiting.graph, Step::Waiting);
    assert_eq!(waiting.mapping, Step::Waiting);
    assert!(waiting.draft.research.is_empty());
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &CredentialRef| None,
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
    let again = bench.start_onboarding().await;
    assert_eq!(again, project);
    let retried = bench.view(&project).await;
    assert_eq!(retried.graph, Step::Waiting);
    assert_eq!(retried.mapping, Step::Waiting);
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
        move |_: &CredentialRef| Some(account.clone()),
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
        move |_: &CredentialRef| Some(account.clone()),
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
        |_: &CredentialRef| None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_onboarded_folder_in_the_home_shows_as_onboarding_then_ready() {
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
        |_: &CredentialRef| None,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    let settled = bench.owner.found_projects().await.unwrap();
    assert_eq!(
        listed(&settled),
        vec![(FoundState::Ready, Some(project), true)]
    );
    bench.stop().await;
}
