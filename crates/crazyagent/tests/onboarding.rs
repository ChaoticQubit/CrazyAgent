#![cfg(unix)]

mod research_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerMutation, OwnerResponse, OwnerResult};
use crazyagent::onboarding::service::{
    CODEX_MISSING, OVERVIEW_BLOCKED, OnboardingLimits, OnboardingService, installed_graphify,
};
use crazyagent::onboarding::{OnboardingView, Step, project_id_for};
use crazyagent::plugins::Scope;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchJob, ResearchStatus};
use crazyagent::projects_home::{FoundProject, FoundState};
use crazyagent::protocol::{CredentialRef, ProjectId};
use crazyagent::research::accounts::{AccountPick, SettingsResolver};
use crazyagent::research::runner::{
    ResearchAccount, ResearchHarness, ResearchLimits, ResearchRunner,
};
use crazyagent::setup::PackState;
#[cfg(target_os = "macos")]
use research_support::hanging_endpoint;
use research_support::{claude, codex, processes_with};
use std::fs;
#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Child;
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POOL: &str = "local-pool";
const PROJECT: &str = "demo-app";
const GRAPHIFY_MISSING: &str = "graphify not installed";
const NO_ACCOUNT: &str = "No signed-in account in this pool can map the project";
const MODEL: &str = "qwen2.5:7b";
#[cfg(target_os = "macos")]
const OLLAMA: &str = "127.0.0.1:11434";
#[cfg(target_os = "macos")]
const OLLAMA_TIME: Duration = Duration::from_secs(10);
#[cfg(target_os = "macos")]
const OLLAMA_TAGS_BYTES_MAX: u64 = 1024 * 1024;
const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:7b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"demo-app\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";
const SAVED_DRAFT: &str = r#"{"name":"Demo Tool","draft":{"about":"Kept by the owner.","tech_stack":["Postgres"],"platforms":["cli"],"research":[],"pool":null,"team":null,"documents":[]},"facts":{"languages":[],"build_commands":[],"test_commands":[],"entry_points":[]}}"#;
const OWNER_OVERVIEW: &str = "# Owner overview\n\nWritten by hand.\n";
const OWNER_ABOUT: &str = "A browser helper the owner described by hand.";
const OWNER_NAME: &str = "Demo Browser";
const FORGED_BRIEF: &str = "# Forged brief\n\nWritten by a project agent.\n";

/// Fails the test with the command that fixes it unless the local Ollama serves the pool
/// model. Input: none; reads the model list from Ollama's `/api/tags`. Output: none.
#[cfg(target_os = "macos")]
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
    matches!(
        step,
        Step::Done | Step::Off | Step::NotInstalled | Step::Failed { .. }
    )
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
#[cfg(target_os = "macos")]
struct EngineProcess {
    child: Child,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
    Binary(EngineProcess),
}

/// A real engine with the local pool in its settings, its owner client and a
/// real git repository outside the engine's runtime folder.
struct Bench {
    _runtime: tempfile::TempDir,
    _repos: tempfile::TempDir,
    skills: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    global: PathBuf,
    project_settings: PathBuf,
    engine: Engine,
    owner: OwnerController,
    generation: u64,
}

/// The private runtime folder with the settings, the committed repo beside
/// it, and a folder outside both for skills the owner already has. `prepare`
/// may add files to the repo.
struct Folders {
    runtime: tempfile::TempDir,
    repos: tempfile::TempDir,
    skills: tempfile::TempDir,
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
        let skills = tempfile::Builder::new()
            .prefix("ca-onboard-skills-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        Self {
            runtime,
            repos,
            skills,
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
            skills: folders.skills,
            root: folders.root,
            repo: folders.repo,
            global: folders.global,
            project_settings: folders.project_settings,
            engine,
            owner,
            generation: 0,
        }
    }

    /// Starts an in-process engine with the Graphify skill already in its
    /// plugin store, so the graph step runs; `prepare` may add files to the repo.
    async fn start(prepare: impl FnOnce(&Path)) -> Self {
        let bench = Self::start_without_graphify(prepare).await;
        bench.graphify_skill().await;
        bench
    }

    /// Adds a global Graphify skill from a folder outside the projects folder,
    /// the way an owner who has the skill already would. The pack then reads it
    /// installed. An unknown outcome is reconciled the way a client does, any other
    /// error fails at once.
    async fn graphify_skill(&self) {
        let folder = self.skills.path().canonicalize().unwrap().join("graphify");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("SKILL.md"),
            "---\nname: graphify\ndescription: Reads the project's code graph.\n---\n",
        )
        .unwrap();
        match self
            .owner
            .add_skill(Scope::Global, folder.to_str().unwrap().into())
            .await
        {
            Ok(_) => {}
            Err(error) if error.0 == "plugins_outcome_unknown" => self.graphify_listed().await,
            Err(error) => panic!("adding the graphify skill failed: {error:?}"),
        }
    }

    /// Waits, at most 30 s, until the global skill list holds graphify. On a loaded machine
    /// the engine stops waiting for storage and answers plugins_outcome_unknown while the
    /// add still runs there, so the client reads the list and never sends the add again.
    /// A list read with the same unknown outcome is read again; any other error fails.
    async fn graphify_listed(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let read = self.owner.plugin_skills(None).await;
            match &read {
                Ok(skills) if skills.iter().any(|skill| skill.name == "graphify") => return,
                Ok(_) => {}
                Err(error) if error.0 == "plugins_outcome_unknown" => {}
                Err(error) => panic!("reading the global skills failed: {error:?}"),
            }
            assert!(
                Instant::now() < until,
                "graphify was never listed after an unknown add outcome: {read:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Starts an in-process engine with nothing of the Graphify pack entry
    /// installed; `prepare` may add files to the repo.
    async fn start_without_graphify(prepare: impl FnOnce(&Path)) -> Self {
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
    #[cfg(target_os = "macos")]
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
        let bench = Self::with(folders, Engine::Binary(process), owner);
        bench.graphify_skill().await;
        bench
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

    /// Stops the in-process engine and starts a new one on the same runtime
    /// folder, as the owner's next launch does. Returns the bench on it.
    async fn restart(self) -> Self {
        self.restart_with(|_| {}).await
    }

    /// Restarts like [`Bench::restart`] and runs `between` on the runtime
    /// folder while no engine is running.
    async fn restart_with(self, between: impl FnOnce(&Path)) -> Self {
        let Self {
            _runtime,
            _repos,
            skills,
            root,
            repo,
            global,
            project_settings,
            engine,
            owner,
            generation,
        } = self;
        drop(owner);
        let (mut service, _stop) = match engine {
            Engine::InProcess(service, stop) => (service, stop),
            #[cfg(target_os = "macos")]
            Engine::Binary(_) => panic!("a restart needs the in-process engine"),
        };
        service.shutdown().await.unwrap();
        between(&root);
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
            _runtime,
            _repos,
            skills,
            root,
            repo,
            global,
            project_settings,
            engine: Engine::InProcess(Box::new(service), stop),
            owner,
            generation,
        }
    }

    /// The repositories the engine stored in the project's specification,
    /// read from its database.
    fn repositories(&self, project: &ProjectId) -> Vec<String> {
        let db = rusqlite::Connection::open_with_flags(
            self.root.join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        let body: String = db
            .query_row(
                "SELECT body FROM projects WHERE id=?1",
                [project.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        serde_json::from_value(body["spec"]["repositories"].clone()).unwrap()
    }

    /// The draft text the engine keeps on the project's onboarding row, read
    /// from its database.
    fn drafted(&self, project: &ProjectId) -> String {
        let db = rusqlite::Connection::open_with_flags(
            self.root.join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.query_row(
            "SELECT drafted FROM onboarding WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// The bytes, inode and modified time of the draft file in the repo,
    /// read without following a link.
    fn draft_file(&self) -> (Vec<u8>, u64, std::time::SystemTime) {
        use std::os::unix::fs::MetadataExt;
        let file = self
            .repo
            .join(".crazyagent")
            .join("setup")
            .join("draft.json");
        let metadata = fs::symlink_metadata(&file).unwrap();
        assert!(metadata.file_type().is_file());
        (
            fs::read(&file).unwrap(),
            metadata.ino(),
            metadata.modified().unwrap(),
        )
    }

    /// Saves the owner's setup for `project` from the review form: the
    /// onboarding draft with the owner's name, about text and platforms.
    async fn save_setup(&mut self, project: &ProjectId, draft: &ProjectSetup) {
        let saved = self
            .owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap();
        let mut setup = draft.clone();
        setup.about = OWNER_ABOUT.into();
        setup.platforms = vec![Platform::Cli];
        self.generation += 1;
        let response = self
            .owner
            .update_project_setup(
                project.clone(),
                saved.revision,
                Some(OWNER_NAME.into()),
                setup,
                self.generation,
            )
            .await
            .unwrap();
        assert_eq!(rejection(&response), None, "{response:?}");
    }

    async fn stop(self) {
        let Self { engine, owner, .. } = self;
        drop(owner);
        match engine {
            Engine::InProcess(mut service, _stop) => service.shutdown().await.unwrap(),
            #[cfg(target_os = "macos")]
            Engine::Binary(mut process) => {
                let clean = tokio::task::spawn_blocking(move || process.terminate())
                    .await
                    .unwrap();
                assert!(clean, "engine without codex did not stop cleanly");
            }
        }
    }
}

/// The pick a test's account closure answers with: `account` on a stand-in pool
/// connection id. Takes the account. Returns the pick.
fn picked(account: &ResearchAccount) -> AccountPick {
    AccountPick::Account {
        connection: CredentialRef::new("test-connection").unwrap(),
        account: Box::new(account.clone()),
    }
}

async fn stop_service(service: OnboardingService) {
    tokio::task::spawn_blocking(move || service.stop())
        .await
        .unwrap();
}

/// A local model endpoint that answers every request with HTTP 400 and an
/// OpenAI-style error naming the missing model, so a real Codex run fails at
/// once. Returns the port; the serving thread ends with the test process.
#[cfg(target_os = "macos")]
fn rejecting_endpoint() -> u16 {
    let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in crazyagent::spawn_gate::incoming_tcp_std(&listener).take(64) {
            let Ok(stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(stream);
            let mut length = 0_usize;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0_u8; length.min(1 << 20)];
            let _ = reader.read_exact(&mut body);
            let error = br#"{"error":{"message":"The model closed-model does not exist","type":"invalid_request_error"}}"#;
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                error.len()
            );
            let _ = stream.write_all(error);
        }
    });
    port
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

#[cfg(target_os = "macos")]
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
    assert_eq!(view.folder_overview, None);
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
    assert_eq!(waiting.graph, Step::WAITING);
    assert_eq!(waiting.mapping, Step::WAITING);
    assert!(waiting.draft.research.is_empty());
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
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
    assert_eq!(retried.graph, Step::WAITING);
    assert_eq!(retried.mapping, Step::WAITING);
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
        (Step::WAITING, Step::WAITING)
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
async fn plan_roadmap_now_waits_until_the_onboarding_setup_is_confirmed() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let before = bench
        .owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!before.saved(), "the setup review is still open");
    let jobs = |jobs: Vec<ResearchJob>| -> Vec<(i64, ResearchStatus)> {
        jobs.into_iter().map(|job| (job.id, job.status)).collect()
    };
    let jobs_before = jobs(bench.owner.research_jobs(project.clone()).await.unwrap());

    bench.generation += 1;
    let response = bench
        .owner
        .plan_roadmap(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_eq!(
        rejection(&response).as_deref(),
        Some("setup_unconfirmed"),
        "{response:?}"
    );
    let after = bench
        .owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.revision, before.revision, "nothing is saved");
    assert_eq!(after.research, before.research);
    assert!(!after.saved());
    assert_eq!(
        jobs(bench.owner.research_jobs(project.clone()).await.unwrap()),
        jobs_before,
        "no planning job is queued"
    );
    let entry = bench.home_entry().await;
    assert_eq!(
        entry.state,
        FoundState::Onboarding,
        "the onboarding stays unfinished: {entry:?}"
    );

    bench.save_setup(&project, &before.editable()).await;
    bench.generation += 1;
    let response = bench
        .owner
        .plan_roadmap(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_ne!(
        rejection(&response).as_deref(),
        Some("setup_unconfirmed"),
        "a confirmed setup plans: {response:?}"
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_a_repo_in_place_onboards_it_where_it_is_and_keeps_its_worktree_links() {
    let mut bench = Bench::start(|repo| {
        let side = repo.parent().unwrap().join("demo-side");
        git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                side.to_str().unwrap(),
            ],
        );
    })
    .await;
    let side = bench.repo.parent().unwrap().join("demo-side");
    let link = fs::read_to_string(side.join(".git")).unwrap();
    let project = bench.start_onboarding().await;
    let waiting = bench.view(&project).await;
    assert_eq!(waiting.project_id, project);
    assert!(bench.repo.join(".git").is_dir());
    assert!(bench.repo.join(".crazyagent").is_dir());
    assert_eq!(fs::read_to_string(side.join(".git")).unwrap(), link);
    assert_eq!(git(&side, &["rev-parse", "--abbrev-ref", "HEAD"]), "side\n");
    assert!(git(&bench.repo, &["worktree", "list"]).contains("demo-side"));
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unticking_graphify_in_setup_installs_nothing_and_the_graph_is_not_installed() {
    let mut bench = Bench::start_without_graphify(|_| {}).await;
    bench.generation += 1;
    let ran = bench
        .owner
        .run_setup(Vec::new(), bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&ran), None, "{ran:?}");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        let status = bench.owner.setup_status().await.unwrap();
        if !status.running {
            break status;
        }
        assert!(Instant::now() < deadline, "setup still running: {status:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let graphify = status
        .pack
        .iter()
        .find(|entry| entry.id == "graphify")
        .unwrap();
    assert_eq!(graphify.state, PackState::Available, "{status:?}");
    let command = status
        .dependencies
        .iter()
        .find(|tool| tool.name == crazyagent::setup::Dependency::Graphify)
        .unwrap();
    assert!(
        !matches!(
            command.state,
            crazyagent::setup::DependencyState::Installing
                | crazyagent::setup::DependencyState::Installed { .. }
        ),
        "{command:?}"
    );
    assert!(
        bench
            .owner
            .plugin_skills(None)
            .await
            .unwrap()
            .iter()
            .all(|skill| skill.name != "graphify")
    );

    let project = bench.start_onboarding().await;
    assert_eq!(bench.view(&project).await.graph, Step::NotInstalled);
    let (runner, _) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
        installed_graphify,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.graph, Step::NotInstalled, "{view:?}");
    assert_eq!(view.mapping, failed(NO_ACCOUNT), "{view:?}");
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert!(!bench.repo.join("graphify-out").exists());
    let again = bench.start_onboarding().await;
    assert_eq!(again, project);
    let retried = bench.view(&project).await;
    assert_eq!(retried.graph, Step::NotInstalled);
    assert_eq!(retried.mapping, Step::WAITING);
    bench.graphify_skill().await;
    assert_eq!(bench.start_onboarding().await, project);
    assert_eq!(
        bench.view(&project).await.graph,
        Step::WAITING,
        "a second start after the skill is installed builds the graph"
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_graphify_skill_switched_off_in_setup_turns_the_graph_off_until_it_is_on_again() {
    let mut bench = Bench::start(|_| {}).await;
    bench.generation += 1;
    let version = bench.owner.setup_status().await.unwrap().version;
    let off = bench
        .owner
        .set_pack_entry("graphify".into(), false, version, bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&off), None, "{off:?}");
    let project = bench.start_onboarding().await;
    assert_eq!(bench.view(&project).await.graph, Step::Off);
    bench.generation += 1;
    let on = bench
        .owner
        .set_pack_entry("graphify".into(), true, version + 1, bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&on), None, "{on:?}");
    assert_eq!(bench.start_onboarding().await, project);
    assert_eq!(bench.view(&project).await.graph, Step::WAITING);
    bench.stop().await;
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mapping_agent_that_runs_out_of_time_leaves_no_process() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        || None,
        OnboardingLimits {
            graph_time: Duration::from_secs(60),
            mapping_time: Duration::from_secs(8),
            mapping_try_time: Duration::from_secs(8),
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

/// A mapping try that runs out of its own time or fails starts a fresh try on a new pick
/// of the pool, with no process left behind: while a later try runs, the view names it and
/// the earlier try's reason, and once all three failed the step names each try's reason,
/// well inside the step's time.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mapping_try_that_runs_out_of_time_starts_a_fresh_try_and_the_step_names_every_try() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (hanging, _server) = hanging_endpoint();
    let rejecting = rejecting_endpoint();
    let endpoint = |port: u16, model: &str| ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: model.into(),
        key: None,
    };
    let accounts = [
        endpoint(hanging, "hanging-model"),
        endpoint(rejecting, "closed-model"),
        endpoint(hanging, "hanging-model"),
    ];
    let picks = std::sync::atomic::AtomicUsize::new(0);
    let (runner, agents) = bench.runner();
    let started = Instant::now();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| {
            let pick = picks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            picked(&accounts[pick.min(accounts.len() - 1)])
        },
        || None,
        OnboardingLimits {
            graph_time: Duration::from_secs(60),
            mapping_time: Duration::from_secs(120),
            mapping_try_time: Duration::from_secs(8),
        },
        Duration::from_millis(50),
    );
    let mut third = None;
    let deadline = Instant::now() + Duration::from_secs(120);
    let view = loop {
        let view = bench.view(&project).await;
        if let Some(progress) = view
            .mapping_try
            .clone()
            .filter(|progress| progress.number == 3)
        {
            third = Some(progress);
        }
        if settled(&view.mapping) {
            break view;
        }
        assert!(
            Instant::now() < deadline,
            "mapping did not settle: {view:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let took = started.elapsed();
    stop_service(service).await;
    assert_eq!(
        view.mapping,
        failed(
            "Mapping agent failed 3 tries: try 1: Mapping agent ran out of time; try 2: Agent stopped: The model closed-model does not exist; try 3: Mapping agent ran out of time"
        ),
        "{view:?}"
    );
    assert_eq!(view.mapping_try, None);
    assert!(
        third.is_some(),
        "the view never showed the third try while it ran"
    );
    let third = third.unwrap();
    assert_eq!((third.number, third.tries), (3, 3));
    assert_eq!(
        third.earlier.as_deref(),
        Some("Agent stopped: The model closed-model does not exist")
    );
    assert!(took < Duration::from_secs(60), "{took:?}");
    assert_eq!(agent_runs(&agents, "onboard-m-"), 3);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_service_mid_mapping_stops_the_agent_and_requeues_the_step() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
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
    assert_eq!(view.mapping, Step::WAITING);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draft_brief_or_overview_the_folder_came_with_is_never_taken_as_the_engines() {
    let mut bench = Bench::start(|repo| {
        fs::create_dir_all(repo.join(".crazyagent/setup")).unwrap();
        fs::create_dir_all(repo.join(".crazyagent/context")).unwrap();
        fs::write(repo.join(".crazyagent/setup/draft.json"), SAVED_DRAFT).unwrap();
        fs::write(repo.join(".crazyagent/overview.md"), OWNER_OVERVIEW).unwrap();
        fs::write(repo.join(".crazyagent/context/brief.md"), FORGED_BRIEF).unwrap();
    })
    .await;
    let project = bench.start_onboarding().await;
    let fresh = bench.view(&project).await;
    assert_eq!(fresh.name, PROJECT);
    assert_eq!(fresh.draft.about, "");
    assert!(fresh.draft.tech_stack.is_empty());
    assert!(fresh.draft.platforms.is_empty());
    assert_eq!(
        fresh.draft.pool.as_ref().map(CredentialRef::as_str),
        Some(POOL)
    );
    assert_eq!(
        fresh.overview, None,
        "the folder's overview is not the mapping agent's"
    );
    assert_eq!(
        fresh.folder_overview.as_deref(),
        Some(OWNER_OVERVIEW.trim())
    );
    let setup = bench
        .owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((setup.name.as_str(), setup.about.as_str()), (PROJECT, ""));
    let brief = fs::read_to_string(bench.repo.join(".crazyagent/context/brief.md")).unwrap();
    assert!(!brief.contains("Forged brief"), "{brief}");
    assert!(!brief.contains("Kept by the owner."), "{brief}");

    let (runner, _) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.draft.about, "");
    assert_eq!(view.draft.tech_stack, ["Rust"]);
    assert_eq!(
        fs::read_to_string(bench.repo.join(".crazyagent/overview.md")).unwrap(),
        OWNER_OVERVIEW
    );

    bench.generation += 1;
    let removed = bench
        .owner
        .remove_project(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&removed), None, "{removed:?}");
    let again = bench.start_onboarding().await;
    let reused = bench.view(&again).await;
    assert_eq!(
        reused.draft.tech_stack,
        ["Rust"],
        "the draft this engine saved is reused when the folder is added again"
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_crazyagent_folder_never_stands_in_for_the_onboarded_context() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let state = bench.repo.join(".crazyagent");
    let brief = fs::read_to_string(state.join("context/brief.md")).unwrap();
    assert!(!brief.contains(FORGED_BRIEF));
    let forged = state.join(".crazyagent/context");
    fs::create_dir_all(forged.join("research")).unwrap();
    fs::write(forged.join("brief.md"), FORGED_BRIEF).unwrap();
    fs::write(forged.join("research/market.md"), FORGED_BRIEF).unwrap();
    let setup = bench
        .owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(setup.state_folder(), state);
    let context = bench.owner.project_context(project.clone()).await.unwrap();
    assert_eq!(Path::new(&context.folder), state);
    let paths: Vec<&str> = context
        .items
        .iter()
        .map(|item| item.path.as_str())
        .collect();
    assert_eq!(paths, ["context/brief.md"]);
    let chunk = bench
        .owner
        .project_context_item(project, "context/brief.md".into(), 0)
        .await
        .unwrap();
    assert_eq!(chunk.text.as_deref(), Some(brief.as_str()));
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
#[cfg(target_os = "macos")]
fn graphify() -> PathBuf {
    installed_graphify().expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)")
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_codex_or_claude_mapping_fails_before_any_account_is_picked() {
    let graphify = graphify();
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (port, _server) = hanging_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "hanging-model".into(),
        key: None,
    };
    let asked = Arc::new(Mutex::new(0_usize));
    let (runner, agents) = bench.runner_without_codex(false);
    let service = {
        let asked = asked.clone();
        OnboardingService::start(
            runner,
            move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| {
                *asked.lock().unwrap() += 1;
                picked(&account)
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
async fn an_overview_entry_that_is_not_a_text_file_fails_mapping_before_any_account_is_picked() {
    let mut bench = Bench::start(|repo| {
        fs::create_dir_all(repo.join(".crazyagent")).unwrap();
        fs::write(repo.join(".crazyagent/overview.md"), "").unwrap();
    })
    .await;
    let project = bench.start_onboarding().await;
    let asked = Arc::new(Mutex::new(0_usize));
    let (runner, agents) = bench.runner();
    let service = {
        let asked = asked.clone();
        OnboardingService::start(
            runner,
            move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| {
                *asked.lock().unwrap() += 1;
                AccountPick::NoUsableAccount
            },
            || None,
            OnboardingLimits::default(),
            Duration::from_millis(50),
        )
    };
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.mapping, failed(OVERVIEW_BLOCKED), "{view:?}");
    assert_eq!(*asked.lock().unwrap(), 0);
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert_eq!(
        fs::read(bench.repo.join(".crazyagent/overview.md")).unwrap(),
        b""
    );
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
            move |_: &str, _: &CredentialRef, harnesses: &[ResearchHarness]| {
                seen.lock().unwrap().push(harnesses.to_vec());
                picked(&ResearchAccount::Endpoint {
                    base_url: "http://127.0.0.1:9/v1".into(),
                    model: "codex-only-model".into(),
                    key: None,
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
        key: None,
    };
    let local = AccountPick::Account {
        connection: CredentialRef::new("local-connection").unwrap(),
        account: Box::new(local),
    };
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &[ResearchHarness::Codex]),
        local.clone()
    );
    assert_eq!(
        resolver.pool_pick(
            "project-a",
            &pool,
            &[ResearchHarness::ClaudeCode, ResearchHarness::Codex]
        ),
        local
    );
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &[ResearchHarness::ClaudeCode]),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &[]),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        resolver.pool_pick(
            "project-a",
            &CredentialRef::new("absent-pool").unwrap(),
            &[ResearchHarness::Codex]
        ),
        AccountPick::NeedsPool
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
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
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

/// Leaves an onboarding row for `PROJECT` at `folder` with no project row, as a
/// project removal made before removals took the onboarding with them.
fn leftover_onboarding(root: &Path, folder: &Path) {
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    database
        .execute(
            "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
             VALUES (?1,?2,?3,'done','done','{}',1,1)",
            [PROJECT, folder.to_str().unwrap(), POOL],
        )
        .unwrap();
}

fn onboarding_repo(root: &Path) -> String {
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    database
        .query_row(
            "SELECT repo FROM onboarding WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leftover_onboarding_of_a_vanished_folder_with_the_same_name_is_replaced() {
    let mut bench = Bench::start(|_| {}).await;
    let gone = bench
        .repo
        .parent()
        .unwrap()
        .join("moved-away")
        .join(PROJECT);
    assert!(!gone.exists());
    leftover_onboarding(&bench.root, &gone);
    assert_eq!(project_rows(&bench.root, "project_setups"), 0);

    let project = bench.start_onboarding().await;
    assert_eq!(onboarding_repo(&bench.root), bench.repo.to_str().unwrap());
    assert_eq!(project_rows(&bench.root, "onboarding"), 1);
    let setup = bench.owner.project_setup(project.clone()).await.unwrap();
    assert_eq!(setup.map(|setup| setup.project_id), Some(project.clone()));
    let view = bench.view(&project).await;
    assert_eq!(view.project_id, project);
    assert!(!view.finished, "{view:?}");
    assert!(bench.repo.join(".crazyagent/context/brief.md").is_file());
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leftover_onboarding_of_another_folder_still_there_refuses_with_a_plain_code() {
    let mut bench = Bench::start(|_| {}).await;
    let other = bench.repo.parent().unwrap().join("elsewhere").join(PROJECT);
    fs::create_dir_all(&other).unwrap();
    leftover_onboarding(&bench.root, &other);

    let repo = bench.repo.clone();
    let response = bench.start_onboarding_at(&repo).await;
    assert_eq!(
        rejection(&response).as_deref(),
        Some("onboarding_project_mismatch"),
        "{response:?}"
    );
    assert_eq!(onboarding_repo(&bench.root), other.to_str().unwrap());
    assert_eq!(project_rows(&bench.root, "project_setups"), 0);
    assert!(!bench.repo.join(".crazyagent").exists());
    assert!(other.is_dir());
    bench.stop().await;
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
             INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms) VALUES ('{PROJECT}','market_research','codex',0,'failed',1);
             INSERT INTO team_finished_view(project,show) VALUES ('{PROJECT}',1);"
        ))
        .unwrap();
    drop(database);
    for table in [
        "onboarding",
        "team_messages",
        "team_tasks",
        "research_jobs",
        "team_finished_view",
    ] {
        assert_eq!(project_rows(&bench.root, table), 1, "{table}");
    }

    bench.generation += 1;
    let response = bench
        .owner
        .remove_project(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&response), None, "{response:?}");
    for table in [
        "onboarding",
        "team_messages",
        "team_tasks",
        "research_jobs",
        "team_finished_view",
    ] {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saving_the_setup_after_mapping_fails_finishes_onboarding_and_reopening_shows_it() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let (runner, _) = bench.runner_without_codex(false);
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.mapping, failed(CODEX_MISSING), "{view:?}");
    assert!(!view.finished, "{view:?}");
    assert!(view.missing.contains(&"about".to_owned()), "{view:?}");
    assert_eq!(bench.home_entry().await.state, FoundState::Onboarding);

    bench.save_setup(&project, &view.draft).await;

    let found = bench.owner.found_projects().await.unwrap();
    let entries: Vec<_> = found
        .iter()
        .filter(|entry| Path::new(&entry.path) == bench.repo)
        .map(|entry| (entry.state, entry.project_id.clone()))
        .collect();
    assert_eq!(
        entries,
        vec![(FoundState::Ready, Some(project.clone()))],
        "a saved setup finishes onboarding: {found:?}"
    );
    let reopened = bench.view(&project).await;
    assert!(reopened.finished, "{reopened:?}");
    assert_eq!(reopened.name, OWNER_NAME);
    assert_eq!(reopened.draft.about, OWNER_ABOUT);
    assert_eq!(reopened.draft.platforms, [Platform::Cli]);
    assert!(
        !reopened.missing.contains(&"about".to_owned()),
        "{reopened:?}"
    );
    assert_eq!(reopened.mapping, failed(CODEX_MISSING));

    let mut bench = bench.restart().await;
    assert_eq!(bench.home_entry().await.state, FoundState::Ready);
    assert_eq!(bench.view(&project).await.draft.about, OWNER_ABOUT);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setup_saved_before_mapping_starts_keeps_the_mapping_agent_from_running() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let draft = bench.view(&project).await.draft;
    bench.save_setup(&project, &draft).await;
    let (runner, _) = bench.runner_without_codex(false);
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !settled(&bench.view(&project).await.graph) {
        assert!(Instant::now() < deadline, "the graph step did not finish");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let view = bench.view(&project).await;
    stop_service(service).await;
    assert_eq!(view.graph, failed(GRAPHIFY_MISSING), "{view:?}");
    assert_eq!(
        view.mapping,
        Step::WAITING,
        "a saved setup needs no mapping agent: {view:?}"
    );
    assert!(view.finished, "{view:?}");
    bench.stop().await;
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_mapping_agent_reports_its_own_reason() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let port = rejecting_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "closed-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        || None,
        OnboardingLimits {
            graph_time: Duration::from_secs(60),
            mapping_time: Duration::from_secs(240),
            mapping_try_time: Duration::from_secs(240),
        },
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(300)).await;
    stop_service(service).await;
    let Step::Failed { reason } = &view.mapping else {
        panic!("the mapping agent cannot reach its model: {view:?}");
    };
    assert_eq!(
        reason,
        "Mapping agent failed 3 tries: Agent stopped: The model closed-model does not exist"
    );
    assert_eq!(agent_runs(&agents, "onboard-m-"), 3);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

/// A mapping step whose loopback provider refuses connections stops at once with one plain
/// line naming the provider and the way out: no try runs and no agent is made, so the owner
/// never waits through three tries of a harness that retries a dead port for minutes.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mapping_step_whose_provider_refuses_connections_stops_at_once_with_the_reach_line() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let closed = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "e2e-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let started = Instant::now();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        || None,
        OnboardingLimits {
            graph_time: Duration::from_secs(60),
            mapping_time: Duration::from_secs(30),
            mapping_try_time: Duration::from_secs(30),
        },
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(90)).await;
    let took = started.elapsed();
    stop_service(service).await;
    assert_eq!(
        view.mapping,
        failed(&format!(
            "Can't reach the model provider at 127.0.0.1:{port}. Start it or pick another account in this pool, then retry setup."
        )),
        "{view:?}"
    );
    assert_eq!(view.mapping_try, None);
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

/// Makes the agent state folder `name` under `agents` with a file in it, as an
/// earlier onboarding of a project with the same id left it.
#[cfg(target_os = "macos")]
fn leftover_agent(agents: &Path, name: &str) {
    fs::create_dir_all(agents.join(name).join("home")).unwrap();
    fs::write(agents.join(name).join("stdout.log"), "earlier run\n").unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_folders_left_by_an_earlier_project_with_the_same_id_never_block_a_new_onboarding() {
    let graphify = graphify();
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let port = rejecting_endpoint();
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "closed-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let earlier = [
        format!("onboard-g-{PROJECT}-1"),
        format!("onboard-m-{PROJECT}-2"),
    ];
    for name in &earlier {
        leftover_agent(&agents, name);
    }
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        move || Some(graphify.clone()),
        OnboardingLimits {
            graph_time: Duration::from_secs(240),
            mapping_time: Duration::from_secs(240),
            mapping_try_time: Duration::from_secs(240),
        },
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(540)).await;
    stop_service(service).await;
    assert_eq!(view.graph, Step::Done, "{view:?}");
    assert_eq!(
        view.mapping,
        failed(
            "Mapping agent failed 3 tries: Agent stopped: The model closed-model does not exist"
        ),
        "{view:?}"
    );
    for name in &earlier {
        assert_eq!(
            fs::read_to_string(agents.join(name).join("stdout.log")).unwrap(),
            "earlier run\n"
        );
    }
    assert_eq!(agent_runs(&agents, "onboard-g-"), 2);
    assert_eq!(agent_runs(&agents, "onboard-m-"), 4);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[cfg(target_os = "macos")]
fn onboarding_created_ms(root: &Path) -> i64 {
    let database = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    database
        .query_row(
            "SELECT created_ms FROM onboarding WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_step_that_cannot_start_records_its_cause_code() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "never-asked-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let created = onboarding_created_ms(&bench.root);
    let taken = [
        format!("onboard-g-{PROJECT}-{created}-1"),
        format!("onboard-m-{PROJECT}-{created}-2-t1"),
    ];
    for name in &taken {
        leftover_agent(&agents, name);
    }
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        || Some(PathBuf::from("/usr/bin/true")),
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(
        view.graph,
        failed("Graph build could not start (run_name_used)"),
        "{view:?}"
    );
    assert_eq!(
        view.mapping,
        failed("Mapping agent could not start (run_name_used)"),
        "{view:?}"
    );
    assert_eq!(agent_runs(&agents, "onboard-"), 2);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mapping_on_linux_stops_with_the_plain_unavailable_line() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "linux-model".into(),
        key: None,
    };
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| picked(&account),
        || None,
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(
        view.mapping,
        failed("Team agents and research are not available on Linux in this version."),
        "{view:?}"
    );
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    assert!(processes_with(&agents).is_empty());
    bench.stop().await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_graph_build_on_linux_stops_with_the_plain_unavailable_line() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let (runner, agents) = bench.runner();
    let service = OnboardingService::start(
        runner,
        |_: &str, _: &CredentialRef, _: &[ResearchHarness]| AccountPick::NoUsableAccount,
        || Some(PathBuf::from("/usr/bin/true")),
        OnboardingLimits::default(),
        Duration::from_millis(50),
    );
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(
        view.graph,
        failed("Graph build is not available on Linux in this version."),
        "{view:?}"
    );
    assert_eq!(view.mapping, failed(NO_ACCOUNT), "{view:?}");
    assert!(!bench.repo.join("graphify-out").exists());
    assert_eq!(agent_runs(&agents, "onboard-g-"), 0);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_onboarded_project_can_be_added_again() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    bench.generation += 1;
    let removed = bench
        .owner
        .remove_project(project.clone(), bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&removed), None, "{removed:?}");
    assert_eq!(bench.home_entry().await.state, FoundState::Found);

    let again = bench.start_onboarding().await;
    assert_eq!(again, project);
    let entry = bench.home_entry().await;
    assert_eq!(
        (entry.state, entry.project_id),
        (FoundState::Onboarding, Some(project.clone()))
    );
    assert!(
        bench
            .owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .is_some()
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_added_folder_is_its_project_repository_and_older_projects_are_repaired_on_start() {
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.to_str().unwrap().to_owned();
    assert_eq!(bench.repositories(&project), std::slice::from_ref(&repo));

    let bench = bench
        .restart_with(|root| {
            let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
            let changed = db
                .execute(
                    "UPDATE projects SET body=json_set(body,'$.spec.repositories',json('[]'))
                     WHERE id=?1",
                    [project.as_str()],
                )
                .unwrap();
            assert_eq!(changed, 1);
        })
        .await;
    assert_eq!(bench.repositories(&project), [repo]);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_mapping_rechecks_without_rewriting_its_draft_and_moves_on_once_the_pick_changes()
{
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let pick = Arc::new(Mutex::new(AccountPick::Paused { until_ms: i64::MAX }));
    let asked = Arc::new(Mutex::new(0_usize));
    let (runner, agents) = bench.runner();
    let service = {
        let pick = pick.clone();
        let asked = asked.clone();
        OnboardingService::start(
            runner,
            move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| {
                *asked.lock().unwrap() += 1;
                pick.lock().unwrap().clone()
            },
            || None,
            OnboardingLimits::default(),
            Duration::from_millis(50),
        )
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut first = None;
    while *asked.lock().unwrap() < 3 {
        assert!(
            Instant::now() < deadline,
            "mapping never asked for an account"
        );
        if first.is_none() && *asked.lock().unwrap() >= 1 {
            first = Some((bench.draft_file(), bench.drafted(&project)));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (file, drafted) = first.unwrap();
    assert!(String::from_utf8(file.0.clone()).unwrap().contains("Rust"));
    let view = bench.view(&project).await;
    assert!(!settled(&view.mapping), "{view:?}");
    assert!(view.draft.tech_stack.iter().any(|entry| entry == "Rust"));
    assert_eq!(bench.draft_file(), file);
    assert_eq!(bench.drafted(&project), drafted);
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    *pick.lock().unwrap() = AccountPick::NoUsableAccount;
    let view = bench.settle(&project, Duration::from_secs(60)).await;
    stop_service(service).await;
    assert_eq!(view.mapping, failed(NO_ACCOUNT), "{view:?}");
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mapping_paused_on_every_pool_account_shows_when_the_pause_ends() {
    const UNTIL_MS: i64 = 4_102_444_800_000;
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let asked = Arc::new(Mutex::new(0_usize));
    let (runner, agents) = bench.runner();
    let service = {
        let asked = asked.clone();
        OnboardingService::start(
            runner,
            move |_: &str, _: &CredentialRef, _: &[ResearchHarness]| {
                *asked.lock().unwrap() += 1;
                AccountPick::Paused { until_ms: UNTIL_MS }
            },
            || None,
            OnboardingLimits::default(),
            Duration::from_millis(50),
        )
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while *asked.lock().unwrap() < 3 {
        assert!(
            Instant::now() < deadline,
            "mapping never asked for an account"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let view = bench.view(&project).await;
    stop_service(service).await;
    assert_eq!(agent_runs(&agents, "onboard-m-"), 0);
    assert_eq!(
        serde_json::to_value(&view.mapping).unwrap(),
        serde_json::json!({"kind": "waiting", "until_ms": UNTIL_MS}),
        "{view:?}"
    );
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_home_move_points_a_nested_project_and_its_repository_at_the_moved_folder() {
    let mut bench = Bench::start(|_| {}).await;
    let home = bench.repo.parent().unwrap().to_path_buf();
    fs::create_dir(home.join("org")).unwrap();
    let nested = home.join("org").join("service-a");
    rust_repo(&nested);
    bench.use_repos_as_home().await;
    let response = bench.start_onboarding_at(&nested).await;
    assert_eq!(rejection(&response), None, "{response:?}");
    let project = project_id_for(&nested).unwrap();
    assert_eq!(
        bench.repositories(&project),
        [nested.to_str().unwrap().to_owned()]
    );
    let next_place = tempfile::Builder::new()
        .prefix("ca-onboard-next-")
        .tempdir()
        .unwrap();
    let next = next_place.path().canonicalize().unwrap().join("home");
    fs::create_dir(&next).unwrap();
    bench.generation += 1;
    let response = bench
        .owner
        .move_projects_home(next.to_str().unwrap().to_owned(), true, bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&response), None, "{response:?}");
    let moved = next.join("org").join("service-a");
    assert!(moved.join(".crazyagent").is_dir() && !nested.exists());
    assert_eq!(
        bench.repositories(&project),
        [moved.to_str().unwrap().to_owned()],
        "the master's workspace follows the nested project"
    );
    let db = rusqlite::Connection::open_with_flags(
        bench.root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    let repo: String = db
        .query_row(
            "SELECT repo FROM onboarding WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(Path::new(&repo), moved);
    let body: String = db
        .query_row(
            "SELECT body FROM project_setups WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        Path::new(stored["folder"].as_str().unwrap()).starts_with(&moved),
        "{stored}"
    );
    drop(db);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starting_onboarding_moves_the_instruction_files_and_the_view_lists_them_and_a_rejected_start_moves_nothing()
 {
    const CLAUDE: &str = "# Rules\n\nONBOARD-CLAUDE-RULE-83\n";
    const AGENTS: &str = "ONBOARD-AGENTS-RULE-84\n";
    let mut bench = Bench::start_without_graphify(|repo| {
        fs::write(repo.join("CLAUDE.md"), CLAUDE).unwrap();
        git(repo, &["add", "CLAUDE.md"]);
        git(repo, &["commit", "-q", "-m", "rules"]);
        fs::write(repo.join("AGENTS.md"), AGENTS).unwrap();
    })
    .await;
    bench.use_repos_as_home().await;
    let imported = bench.repo.join(".crazyagent").join("imported");
    let link = bench.root.join("repo-link");
    std::os::unix::fs::symlink(&bench.repo, &link).unwrap();
    let refused = bench.start_onboarding_at(&link).await;
    assert_eq!(
        rejection(&refused).as_deref(),
        Some("onboarding_folder_missing")
    );
    assert_eq!(
        fs::read_to_string(bench.repo.join("CLAUDE.md")).unwrap(),
        CLAUDE
    );
    assert_eq!(
        fs::read_to_string(bench.repo.join("AGENTS.md")).unwrap(),
        AGENTS
    );
    assert!(fs::symlink_metadata(&imported).is_err());
    let project = bench.start_onboarding().await;
    let view = bench.view(&project).await;
    let file =
        |path: &str, bytes: usize, tracked: bool| crazyagent::instruction_files::ImportFile {
            path: path.to_owned(),
            bytes: bytes as u64,
            tracked,
            refused: None,
            uncommitted: tracked,
        };
    assert_eq!(
        view.imported,
        [
            file("CLAUDE.md", CLAUDE.len(), true),
            file("AGENTS.md", AGENTS.len(), false)
        ]
    );
    assert!(fs::symlink_metadata(bench.repo.join("CLAUDE.md")).is_err());
    assert!(fs::symlink_metadata(bench.repo.join("AGENTS.md")).is_err());
    assert_eq!(
        fs::read_to_string(imported.join("CLAUDE.md")).unwrap(),
        CLAUDE
    );
    assert_eq!(
        fs::read_to_string(imported.join("AGENTS.md")).unwrap(),
        AGENTS
    );
    assert_eq!(
        fs::read_to_string(bench.repo.join(".crazyagent").join("instructions.md")).unwrap(),
        "## Imported from CLAUDE.md\n\n# Rules\n\nONBOARD-CLAUDE-RULE-83\n\n## Imported from AGENTS.md\n\nONBOARD-AGENTS-RULE-84\n"
    );
    assert_eq!(
        git(&bench.repo, &["status", "--porcelain"]),
        " D CLAUDE.md\n"
    );
    bench.stop().await;
}

/// A project onboarded from an existing folder runs its agents in their own clones, as a
/// New Project does: onboarding saves the New Project access level wherever this computer
/// can run it, and starting the same folder's onboarding again keeps the level the owner
/// chose since (fix/own-clones-and-lead-landing 6e60b198).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_onboarding_saves_own_branch_access_and_a_restart_keeps_the_owners_level() {
    use crazyagent::sandbox::policy::{AgentAccess, AgentPolicy};
    let mut bench = Bench::start(|_| {}).await;
    let project = bench.start_onboarding().await;
    let saved = bench
        .owner
        .project_agent_policy(project.clone())
        .await
        .unwrap();
    let expected = match AgentAccess::NEW_PROJECT.unavailable() {
        None => AgentAccess::NEW_PROJECT,
        Some(_) => AgentAccess::Sandboxed,
    };
    assert_eq!(saved.access, expected);
    #[cfg(target_os = "macos")]
    assert_eq!(saved.access, AgentAccess::OwnBranch);
    bench.generation += 1;
    let response = bench
        .owner
        .set_project_agent_policy(
            project.clone(),
            saved.version,
            AgentPolicy {
                access: AgentAccess::Sandboxed,
                network: crazyagent::sandbox::policy::AgentNetwork::default(),
                toolchains: Vec::new(),
                ide_git_hooks: false,
                tool_cache: Default::default(),
                ide_agent_files: Default::default(),
                shared_folders: Default::default(),
                ..Default::default()
            },
            bench.generation,
        )
        .await
        .unwrap();
    assert_eq!(rejection(&response), None, "{response:?}");
    let repo = bench.repo.clone();
    let restarted = bench.start_onboarding_at(&repo).await;
    assert_eq!(rejection(&restarted), None, "{restarted:?}");
    let kept = bench.owner.project_agent_policy(project).await.unwrap();
    assert_eq!(kept.access, AgentAccess::Sandboxed);
    bench.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_own_branch_change_set_still_lands_when_the_owner_asks_to_move_the_home() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let repo = bench.repo.clone();
    let response = bench.start_onboarding_at(&repo).await;
    assert_eq!(rejection(&response), None, "{response:?}");
    let project = project_id_for(&repo).unwrap();
    let worktree = crazyagent::projects_home::state_folder(&repo)
        .join("worktrees")
        .join("agent-own");
    assert!(
        worktree.starts_with(repo.join(".crazyagent")),
        "{worktree:?}"
    );
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    crazyagent::landing::add_agent_clone(&repo, &worktree).unwrap();
    let git_dir = crazyagent::agent_git::git_dir(&worktree);
    let db = crazyagent::landing::open(&bench.root).unwrap();
    crazyagent::landing::record_worktree_start(&db, &repo, &worktree, &git_dir).unwrap();
    fs::write(worktree.join("NOTES.md"), "agent notes\n").unwrap();
    git(&worktree, &["add", "NOTES.md"]);
    git(&worktree, &["commit", "-q", "-m", "Add notes"]);
    let workspace = crazyagent::landing::Workspace {
        project: project.clone(),
        agent: crazyagent::landing::ChangeAgent {
            kind: crazyagent::landing::AgentKind::Sub,
            id: "agent-own".into(),
            label: "worker".into(),
        },
        repository: repo.clone(),
        worktree: worktree.clone(),
        git_dir,
    };
    let turn = crazyagent::landing::TurnEnd {
        outcome: crazyagent::landing::TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add notes".into(),
    };
    let id = crazyagent::landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .expect("the agent's commit opens a change set");
    let next_place = tempfile::Builder::new()
        .prefix("ca-onboard-next-")
        .tempdir()
        .unwrap();
    let next = next_place.path().canonicalize().unwrap().join("home");
    fs::create_dir(&next).unwrap();
    bench.generation += 1;
    let moved = bench
        .owner
        .move_projects_home(next.to_str().unwrap().to_owned(), true, bench.generation)
        .await
        .unwrap();
    let head = crazyagent::landing::list(&db, &project)
        .unwrap()
        .into_iter()
        .find(|view| view.id == id)
        .expect("the change set is listed")
        .head;
    let owner_home = tempfile::tempdir().unwrap();
    fs::write(
        owner_home.path().join(".gitconfig"),
        "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n",
    )
    .unwrap();
    let landed = crazyagent::landing::accept(
        &db,
        owner_home.path(),
        &id,
        &head,
        None,
        crazyagent::activity::AcceptPlace::Changes,
    );
    assert!(
        matches!(
            landed,
            Ok(crazyagent::landing::AcceptOutcome::Landed { .. })
        ),
        "after the home move answered {moved:?}, Accept answered {landed:?}"
    );
    assert_eq!(rejection(&moved).as_deref(), Some("home_move_agent_work"));
    assert!(crazyagent::projects_home::plain_problem("home_move_agent_work").is_some());
    assert!(repo.join("NOTES.md").is_file() && !next.join(PROJECT).exists());
    bench.generation += 1;
    let again = bench
        .owner
        .move_projects_home(next.to_str().unwrap().to_owned(), true, bench.generation)
        .await
        .unwrap();
    assert_eq!(rejection(&again), None, "{again:?}");
    assert!(next.join(PROJECT).join("NOTES.md").is_file() && !repo.exists());
    drop(db);
    bench.stop().await;
}

/// A new folder for the projects home outside the bench. Returns its holder and path.
fn next_home() -> (tempfile::TempDir, PathBuf) {
    let place = tempfile::Builder::new()
        .prefix("ca-onboard-next-")
        .tempdir()
        .unwrap();
    let next = place.path().canonicalize().unwrap().join("home");
    fs::create_dir(&next).unwrap();
    (place, next)
}

/// Checks that a home move was refused with `code`, which reads as a plain sentence, and
/// that the bench repository stayed where it was with nothing in the new home.
fn refused_move(moved: &OwnerResponse, code: &str, repo: &Path, next: &Path) {
    assert_eq!(rejection(moved).as_deref(), Some(code), "{moved:?}");
    assert!(
        crazyagent::projects_home::plain_problem(code).is_some(),
        "{code}"
    );
    assert!(repo.join(".git").is_dir() && !next.join(PROJECT).exists());
}

/// Checks that a home move went through and took the bench repository with it.
fn moved_home(moved: &OwnerResponse, repo: &Path, next: &Path) {
    assert_eq!(rejection(moved), None, "{moved:?}");
    assert!(next.join(PROJECT).join(".git").is_dir() && !repo.exists());
}

/// Makes an own clone for the agent `name` in the bench repository and commits a file in
/// it, so the agent's turn opens a change set. Returns the landing store, the change set
/// and the head the owner reviews.
fn agent_change(
    bench: &Bench,
    project: &ProjectId,
    name: &str,
) -> (
    rusqlite::Connection,
    crazyagent::landing::ChangeSetId,
    String,
) {
    let worktree = crazyagent::projects_home::state_folder(&bench.repo)
        .join("worktrees")
        .join(name);
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    crazyagent::landing::add_agent_clone(&bench.repo, &worktree).unwrap();
    let git_dir = crazyagent::agent_git::git_dir(&worktree);
    let db = crazyagent::landing::open(&bench.root).unwrap();
    crazyagent::landing::record_worktree_start(&db, &bench.repo, &worktree, &git_dir).unwrap();
    fs::write(worktree.join("NOTES.md"), "agent notes\n").unwrap();
    git(&worktree, &["add", "NOTES.md"]);
    git(&worktree, &["commit", "-q", "-m", "Add notes"]);
    let workspace = crazyagent::landing::Workspace {
        project: project.clone(),
        agent: crazyagent::landing::ChangeAgent {
            kind: crazyagent::landing::AgentKind::Sub,
            id: name.into(),
            label: "worker".into(),
        },
        repository: bench.repo.clone(),
        worktree,
        git_dir,
    };
    let turn = crazyagent::landing::TurnEnd {
        outcome: crazyagent::landing::TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add notes".into(),
    };
    let id = crazyagent::landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .expect("the agent's commit opens a change set");
    let head = crazyagent::landing::list(&db, project)
        .unwrap()
        .into_iter()
        .find(|view| view.id == id)
        .expect("the change set is listed")
        .head;
    (db, id, head)
}

impl Bench {
    /// Asks the engine to move the projects home, with its projects, to `next`.
    /// Returns the owner response.
    async fn move_home(&mut self, next: &Path) -> OwnerResponse {
        self.generation += 1;
        self.owner
            .move_projects_home(next.to_str().unwrap().to_owned(), true, self.generation)
            .await
            .unwrap()
    }
}

/// A change set the owner discarded stays restorable for 7 days and holds a home move with
/// its own sentence, which says when the move can go; once that time passed the move goes
/// through. The agent work sentence no longer says that a discard is enough.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restorable_discarded_change_holds_a_home_move_until_its_restore_window_ends() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let (db, id, head) = agent_change(&bench, &project, "agent-discard");
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let until = crazyagent::landing::discard(&db, &id, &head, now).unwrap();
    assert!(until > now, "{until} {now}");
    assert_eq!(
        crazyagent::landing::RESTORE_WINDOW_MS,
        7 * 24 * 60 * 60 * 1000
    );
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_changes_restorable", &repo, &next);
    for code in ["home_move_changes_restorable", "home_move_agent_work"] {
        let sentence = crazyagent::projects_home::plain_problem(code).unwrap();
        assert!(sentence.contains("7 days"), "{code}: {sentence}");
    }
    db.execute(
        "UPDATE landing_change_sets SET restorable_until_ms=?1 WHERE id=?2",
        rusqlite::params![i64::try_from(now).unwrap() - 1, id.as_str()],
    )
    .unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}

/// A research job that runs for a project in the home holds a home move with its own
/// sentence, since its worktree in the project would be cut off; once the job ended the
/// move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_research_job_holds_a_home_move_until_it_ends() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let jobs =
        crazyagent::research::jobs::ResearchJobs::open(&bench.root.join("engine.db")).unwrap();
    jobs.enqueue(project.as_str(), "stack", "codex", false)
        .unwrap();
    let job = jobs
        .claim(project.as_str(), "test-runner", 4)
        .unwrap()
        .expect("a queued job is claimed");
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_research_running", &repo, &next);
    jobs.finish(&job, "context/research/stack.md").unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(jobs);
    bench.stop().await;
}

/// Opens the bench engine's database for writing the agent records a test stands in for.
fn engine_db(root: &Path) -> rusqlite::Connection {
    let db = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db
}

/// A team agent that is starting in a project in the home holds a home move, since its
/// worktree is about to be made there; once it stopped the move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_starting_in_the_home_holds_a_home_move() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let db = engine_db(&bench.root);
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',0,'local-connection','starting',1,1)",
        [project.as_str()],
    )
    .unwrap();
    let agent = db.last_insert_rowid();
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_agent_work", &repo, &next);
    db.execute(
        "UPDATE team_agents SET status='failed', stopped_ms=2 WHERE id=?1",
        [agent],
    )
    .unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}

/// A projects home move recorded before its folders move holds every research launch in
/// the home: a job claimed meanwhile goes back to the queue without spending its attempt,
/// and runs on once the move is finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_research_job_claimed_while_the_home_moves_goes_back_to_wait() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let repo = bench.repo.clone();
    let db = engine_db(&bench.root);
    db.execute(
        "INSERT INTO projects_home_move VALUES (1,'request-held',1,?1,?2,'[\"demo-app\"]')",
        rusqlite::params![
            repo.parent().unwrap().to_str().unwrap(),
            bench.root.join("next").to_str().unwrap()
        ],
    )
    .unwrap();
    let jobs =
        crazyagent::research::jobs::ResearchJobs::open(&bench.root.join("engine.db")).unwrap();
    let id = jobs
        .enqueue("held-research", "stack", "codex", false)
        .unwrap();
    let (runner, _) = bench.runner_without_codex(false);
    let project = crazyagent::research::runner::ResearchProject {
        id: "held-research",
        root: &repo,
        account: None,
    };
    let held = runner
        .run_once(&project)
        .unwrap()
        .expect("the queued job is claimed");
    assert_eq!(
        (held.job, held.status),
        (id, crazyagent::research::jobs::ResearchStatus::Queued)
    );
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        (job.status, job.attempts),
        (crazyagent::research::jobs::ResearchStatus::Queued, 0)
    );
    db.execute("DELETE FROM projects_home_move WHERE singleton=1", [])
        .unwrap();
    let ran = runner
        .run_once(&project)
        .unwrap()
        .expect("the job is claimed again");
    assert_eq!(
        (ran.job, ran.status),
        (id, crazyagent::research::jobs::ResearchStatus::NeedsPool)
    );
    drop(jobs);
    drop(db);
    bench.stop().await;
}

/// A team agent's own clone recorded in a project in the home holds a home move; once the
/// agent is removed with its clone the move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_agent_clone_in_the_home_holds_a_home_move_until_the_agent_is_removed() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let worktree = crazyagent::projects_home::state_folder(&repo)
        .join("worktrees")
        .join("agent-7");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    crazyagent::landing::add_agent_clone(&repo, &worktree).unwrap();
    let git_dir = crazyagent::agent_git::git_dir(&worktree);
    let db = engine_db(&bench.root);
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (7,?1,?2,?3,?4)",
        rusqlite::params![
            project.as_str(),
            repo.to_str().unwrap(),
            worktree.to_str().unwrap(),
            git_dir.to_str().unwrap()
        ],
    )
    .unwrap();
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_agent_work", &repo, &next);
    db.execute("DELETE FROM team_worktrees WHERE agent=7", [])
        .unwrap();
    fs::remove_dir_all(&worktree).unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}

/// An agent the owner confirmed for removal whose files are not gone yet holds a home move
/// while its record waits; once its removal finished the move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_waiting_for_removal_in_the_home_holds_a_home_move() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let db = engine_db(&bench.root);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    assert!(
        crazyagent::team::cleanup::finished(&db, &project, now)
            .unwrap()
            .is_empty()
    );
    let worktree = crazyagent::projects_home::state_folder(&repo)
        .join("worktrees")
        .join("agent-8");
    fs::create_dir_all(&worktree).unwrap();
    db.execute(
        "INSERT INTO team_retiring(agent,project,repository,worktree,git_dir,since_ms) VALUES (8,?1,?2,?3,?4,?5)",
        rusqlite::params![
            project.as_str(),
            repo.to_str().unwrap(),
            worktree.to_str().unwrap(),
            worktree.join(".git").to_str().unwrap(),
            now
        ],
    )
    .unwrap();
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_agent_work", &repo, &next);
    db.execute("DELETE FROM team_retiring WHERE agent=8", [])
        .unwrap();
    fs::remove_dir_all(&worktree).unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}

/// A landed change set with a worktree step recorded but not finished holds a home move,
/// since an engine start finishes that step at the recorded paths; once no step is left
/// the move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_set_with_an_unfinished_step_holds_a_home_move() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let (db, id, head) = agent_change(&bench, &project, "agent-step");
    let owner_home = tempfile::tempdir().unwrap();
    fs::write(
        owner_home.path().join(".gitconfig"),
        "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n",
    )
    .unwrap();
    let landed = crazyagent::landing::accept(
        &db,
        owner_home.path(),
        &id,
        &head,
        None,
        crazyagent::activity::AcceptPlace::Changes,
    );
    assert!(
        matches!(
            landed,
            Ok(crazyagent::landing::AcceptOutcome::Landed { .. })
        ),
        "{landed:?}"
    );
    let step = serde_json::json!({ "step": "reset", "target": head }).to_string();
    db.execute(
        "UPDATE landing_change_sets SET intent=?1 WHERE id=?2",
        rusqlite::params![step, id.as_str()],
    )
    .unwrap();
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_agent_work", &repo, &next);
    db.execute(
        "UPDATE landing_change_sets SET intent=NULL WHERE id=?1",
        [id.as_str()],
    )
    .unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}

/// A team agent's worktree recorded through a link to the home still holds a home move:
/// the check follows the recorded path to where it is now. Once the record is gone the
/// move goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_worktree_recorded_through_a_link_still_holds_a_home_move() {
    let mut bench = Bench::start(|_| {}).await;
    bench.use_repos_as_home().await;
    let project = bench.start_onboarding().await;
    let repo = bench.repo.clone();
    let home = repo.parent().unwrap().to_path_buf();
    let links = tempfile::tempdir().unwrap();
    let link = links.path().join("repos");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let through = link.join(PROJECT);
    assert!(!through.starts_with(&home) && through.canonicalize().unwrap() == repo);
    let worktree = through.join("agent-9");
    let db = engine_db(&bench.root);
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (9,?1,?2,?3,?4)",
        rusqlite::params![
            project.as_str(),
            through.to_str().unwrap(),
            worktree.to_str().unwrap(),
            worktree.join(".git").to_str().unwrap()
        ],
    )
    .unwrap();
    let (_place, next) = next_home();
    let moved = bench.move_home(&next).await;
    refused_move(&moved, "home_move_agent_work", &repo, &next);
    db.execute("DELETE FROM team_worktrees WHERE agent=9", [])
        .unwrap();
    let again = bench.move_home(&next).await;
    moved_home(&again, &repo, &next);
    drop(db);
    bench.stop().await;
}
