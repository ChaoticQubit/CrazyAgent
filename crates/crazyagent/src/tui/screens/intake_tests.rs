use super::settings_accounts::tests::{ctx, render};
use super::{Place, ProjectTab, open};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::github::{GitHubMethod, GitHubStatus};
use crate::onboarding::{MappingFacts, OnboardingView, Step};
use crate::project_setup::{ProjectSetup, SetupAction, SetupQuery};
use crate::protocol::ProjectId;
use crate::tui::client::Connection;
use crate::tui::client::intake::{IntakeEvent, IntakeJob, IntakeQuery, IntakeView};
use crate::tui::client::{OwnerEvent, SetupEvent};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::glyphs::Status;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::seam::{Arrival, Request, ShellRequest};
use crate::tui::shell::Shell;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const SETTINGS: &str = r#"version = 1
timeout_ms = 10000
allowed_destinations = []

[model_access]
accounts = ["local-1"]
connections = [
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "pool-a", name = "Main", members = ["local-a"], projects = [], roles = [], fallback_allowed = false },
]
"#;

struct Engine {
    root: tempfile::TempDir,
    user: tempfile::TempDir,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    controller: OwnerController,
}

impl Engine {
    async fn start() -> Self {
        Self::with(SETTINGS).await
    }

    async fn with(settings: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-intake-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, settings).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        let client = Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap();
        let (_, controller) = OwnerController::attach_configured(client, OwnerClientKind::Tui)
            .await
            .unwrap();
        let user = tempfile::Builder::new()
            .prefix("ca-tui-intake-user-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(user.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            user,
            _service: service,
            _stop: stop,
            controller,
        }
    }

    fn connect(&self) -> Connection {
        Connection::connect_paired(
            Client::new(
                self.root.path().join("e.sock"),
                crate::tui::test_home::ENGINE_WAIT,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn user(&self) -> std::path::PathBuf {
        std::fs::canonicalize(self.user.path()).unwrap()
    }

    fn home(&self) -> std::path::PathBuf {
        let home = self.user().join("Projects");
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    async fn set_home(&mut self) -> std::path::PathBuf {
        let home = self.home();
        let reply = self
            .controller
            .set_projects_home(home.to_str().unwrap().into(), 1)
            .await
            .unwrap();
        assert!(
            matches!(
                &reply,
                OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
            ),
            "{reply:?}"
        );
        home
    }
}

fn screen_ctx(home: &Path) -> Ctx {
    let mut ctx = ctx(80, 24);
    ctx.home = Some(home.to_str().unwrap().into());
    ctx
}

async fn pump(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
            moved = true;
        }
        if !connection.busy()
            && shell.ctx.link == Link::Ready
            && let Some(request) = shell.ctx.next_request()
        {
            moved = true;
            match request {
                Request::Pools => {
                    sent.push("pools");
                    connection.shell(ShellRequest::Pools).unwrap();
                }
                Request::Projects => {
                    sent.push("projects");
                    connection.shell(ShellRequest::Projects).unwrap();
                }
                Request::Settings(job) => {
                    sent.push("settings");
                    connection.settings(*job).unwrap();
                }
                Request::Setup(query) => {
                    sent.push("setup");
                    connection.project_setup(query).unwrap();
                }
                Request::Intake(job) => {
                    sent.push(match job.query() {
                        IntakeQuery::Home => "home",
                        IntakeQuery::SetHome { .. } => "set home",
                        IntakeQuery::Found => "found",
                        IntakeQuery::FolderSize { .. } => "folder size",
                        IntakeQuery::AddExisting { .. } => "add existing",
                        IntakeQuery::StartOnboarding { .. } => "start onboarding",
                        IntakeQuery::Onboarding { .. } => "onboarding",
                        _ => "github",
                    });
                    connection.intake(*job).unwrap();
                }
                other => panic!("intake pages never send {other:?}"),
            }
        }
        quiet = if moved || connection.busy() {
            0
        } else {
            quiet + 1
        };
        if quiet > 20 {
            return sent;
        }
        assert!(
            Instant::now() < deadline,
            "engine reads never settled: {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn keys(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

fn clear(shell: &mut Shell, count: usize) {
    for _ in 0..count {
        shell.key(Key::Backspace);
    }
}

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

fn intake(query: IntakeQuery, result: Result<IntakeView, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Intake(Box::new(IntakeEvent {
        query,
        result,
    }))))
}

#[test]
fn tab_completes_the_projects_folder_like_a_shell() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("Projects")).unwrap();
    std::fs::create_dir(home.path().join("Pictures")).unwrap();
    std::fs::write(home.path().join("Prose.txt"), "a").unwrap();
    let mut ctx = screen_ctx(home.path());
    let screen = open(&Place::HomeFolder { first_run: true }, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Your projects folder"), "{rows:#?}");
    assert!(shows(&rows, "~/Projects"), "the suggestion is typed in");
    clear(&mut shell, "~/Projects".len());
    keys(&mut shell, "~/P");
    shell.key(Key::Tab);
    let rows = render(&mut shell, 80, 24);
    assert!(
        !shows(&rows, "~/Pictures") && !shows(&rows, "~/Projects"),
        "two folders match the start, so nothing is added: {rows:#?}"
    );
    keys(&mut shell, "r");
    shell.key(Key::Tab);
    let rows = render(&mut shell, 80, 24);
    assert!(
        shows(&rows, "~/Projects/"),
        "one folder matches; the file is never offered"
    );
}

#[test]
fn a_relative_projects_folder_is_refused_before_it_reaches_the_engine() {
    let home = tempfile::tempdir().unwrap();
    let mut ctx = screen_ctx(home.path());
    let screen = open(&Place::HomeFolder { first_run: true }, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    while shell.ctx.next_request().is_some() {}
    clear(&mut shell, "~/Projects".len());
    keys(&mut shell, "Projects");
    shell.key(Key::Enter);
    assert!(shell.ctx.next_request().is_none());
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Type a full folder path"), "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_run_saves_the_projects_folder_and_continues_to_setup() {
    let mut engine = Engine::start().await;
    let home = engine.home();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::HomeFolder { first_run: true }, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"set home"), "{sent:?}");
    assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
    let saved = engine.controller.projects_home().await.unwrap();
    assert_eq!(saved.path.as_deref(), home.to_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_lists_a_found_folder_and_add_project_offers_three_ways() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    std::fs::create_dir(home.join("notes")).unwrap();
    std::fs::write(home.join("notes").join("README.md"), "# Notes\n").unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"found"), "{sent:?}");
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "notes"), "{rows:#?}");
    assert!(shows(&rows, "Not set up"), "{rows:#?}");
    let state = format!("{} Not set up", shell.ctx.glyphs.status(Status::Waiting));
    assert!(shows(&rows, &state), "{rows:#?}");
    assert!(shows(&rows, "a Add project"), "{rows:#?}");

    shell.key(Key::Char('a'));
    assert_eq!(shell.place(), Some(Place::AddProject));
    let rows = render(&mut shell, 80, 24);
    for way in ["New project", "Clone from GitHub", "Existing folder"] {
        assert!(shows(&rows, way), "{way} missing: {rows:#?}");
    }
    shell.key(Key::Char('e'));
    assert_eq!(shell.place(), Some(Place::AddFolder));
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home), "Add project was replaced");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_folder_is_set_up_where_it_is_by_default() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let source = engine.user().join("outside").join("shop");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("README.md"), "shop\n").unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "(*) Use where it is"), "{rows:#?}");
    assert!(shows(&rows, "( ) Move"), "{rows:#?}");
    assert!(shows(&rows, "Stays where it is"), "{rows:#?}");
    assert!(shows(&rows, "Set up here"), "{rows:#?}");
    assert!(!shows(&rows, "Goes to "), "{rows:#?}");
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"add existing"), "{sent:?}");
    assert!(source.join("README.md").exists());
    assert!(!home.join("shop").exists());
    assert_eq!(
        shell.place(),
        Some(Place::Onboarding {
            path: source.canonicalize().unwrap().to_str().unwrap().into(),
            name: "shop".into(),
            project: None,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_folder_shows_its_size_and_a_warning_before_a_copy() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let source = engine.user().join("outside").join("big");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::File::create(source.join("data.bin"))
        .unwrap()
        .set_len(3 << 29)
        .unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "(*) Copy"), "{rows:#?}");
    assert!(shows(&rows, "Goes to "), "{rows:#?}");
    assert!(shows(&rows, "Check size"), "{rows:#?}");
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Size: "), "{rows:#?}");
    assert!(shows(&rows, "over 1 GB"), "{rows:#?}");
    assert!(shows(&rows, "Copy anyway"), "{rows:#?}");

    shell.key(Key::Left);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "(*) Move"), "{rows:#?}");
    assert!(shows(&rows, "Goes to "), "{rows:#?}");
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "won't stay at its old place"), "{rows:#?}");
    assert!(shows(&rows, "Move it now"), "{rows:#?}");
    assert!(
        source.join("data.bin").exists(),
        "nothing moves before the confirm"
    );
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"add existing"), "{sent:?}");
    assert!(home.join("big").join("data.bin").exists());
    assert!(!source.exists(), "a move leaves no original");
    assert_eq!(
        shell.place(),
        Some(Place::Onboarding {
            path: home.join("big").to_str().unwrap().into(),
            name: "big".into(),
            project: None,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn onboarding_starts_on_a_pool_and_shows_both_steps() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let folder = home.join("notes");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join("Cargo.toml"),
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(
        &Place::Onboarding {
            path: folder.to_str().unwrap().into(),
            name: "notes".into(),
            project: None,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["pools"]);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Set up notes"), "{rows:#?}");
    assert!(shows(&rows, "Main"), "{rows:#?}");
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(&sent[..2], ["start onboarding", "onboarding"], "{sent:?}");
    let rows = render(&mut shell, 80, 24);
    assert!(
        shows(&rows, "Build the code graph") || shows(&rows, "Name"),
        "{rows:#?}"
    );
    let Some(Place::Onboarding {
        project: Some(project),
        ..
    }) = shell.place()
    else {
        panic!("the started onboarding names its project");
    };
    let onboarding = engine.controller.onboarding(project).await.unwrap();
    assert_eq!(onboarding.name, "notes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_run_setup_continues_without_installing_and_home_lists_what_is_missing() {
    let mut engine = Engine::with("version = 1\ntimeout_ms = 10000\n").await;
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::Setup { first_run: true }, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["settings"]);
    let rows = render(&mut shell, 80, 24);
    let help = rows.last().unwrap();
    assert!(help.contains("Enter Install and continue"), "{rows:#?}");
    assert!(help.contains("c Continue without installing"), "{rows:#?}");

    shell.key(Key::Char('c'));
    assert_eq!(shell.place(), Some(Place::Home));
    settle_setup(&mut shell, &mut connection).await;
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Get set up"), "{rows:#?}");
    assert!(shows(&rows, "[ ] Add an account"), "{rows:#?}");
    assert!(shows(&rows, "[ ] Create a pool"), "{rows:#?}");
    assert!(
        !engine.root.path().join("setup").exists(),
        "continuing without installing creates no setup folder"
    );
    let status = engine.controller.setup_status().await.unwrap();
    assert!(!status.running, "no setup run started");
    let found = tools_found(&status);
    assert_eq!(
        shows(&rows, "[ ] Install tools"),
        !found,
        "the engine reports {:?}: {rows:#?}",
        status.dependencies
    );
    assert_eq!(shows(&rows, "[x] Install tools"), found, "{rows:#?}");
    assert_eq!(shell.ctx.store.setup, Load::Ready(status));
}

/// Pumps the shell, refreshing like the run loop, until the store holds a setup
/// check with every tool checked.
/// Input: the shell and its engine connection. Output: none; panics after 30 s.
async fn settle_setup(shell: &mut Shell, connection: &mut Connection) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        pump(shell, connection).await;
        if let Load::Ready(status) = &shell.ctx.store.setup
            && status
                .dependencies
                .iter()
                .all(|tool| tool.state != crate::setup::DependencyState::Checking)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the setup check never finished: {:?}",
            shell.ctx.store.setup
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        shell.refresh();
    }
}

fn tools_found(status: &crate::setup::SetupStatus) -> bool {
    status.dependencies.iter().all(|tool| {
        matches!(
            tool.state,
            crate::setup::DependencyState::Found { .. }
                | crate::setup::DependencyState::Installed { .. }
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_hides_get_set_up_only_when_an_account_a_pool_and_tools_are_ready() {
    let mut engine = Engine::start().await;
    engine
        .controller
        .add_account(
            crate::accounts::AccountDraft {
                provider: crate::accounts::Provider::AnthropicApi,
                method: crate::accounts::SignInMethod::ApiKey,
                name: "Team key".into(),
                url: None,
                model: Some("claude-sonnet-4-5".into()),
            },
            Some(
                crate::engine::owner::AccountSecret::new(
                    b"unit-placeholder-anthropic-key-0001".to_vec(),
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    settle_setup(&mut shell, &mut connection).await;
    let status = engine.controller.setup_status().await.unwrap();
    let rows = render(&mut shell, 80, 24);
    let found = tools_found(&status);
    assert_eq!(
        shows(&rows, "Get set up"),
        !found,
        "a pool exists and an account has a saved key, so only tools decide; the engine reports {:?}: {rows:#?}",
        status.dependencies
    );
    assert_eq!(shows(&rows, "[ ] Install tools"), !found, "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_an_account_is_done_only_when_an_engine_account_is_ready_for_agents() {
    use crate::accounts::{AccountDraft, AccountStatus, Provider, SignInMethod};
    use crate::engine::owner::AccountSecret;

    let engine = Engine::with("version = 1\ntimeout_ms = 10000\n").await;
    let draft = |provider, method, name: &str, model: Option<&str>| AccountDraft {
        provider,
        method,
        name: name.into(),
        url: None,
        model: model.map(Into::into),
    };
    for _ in 0..2 {
        let waiting = engine
            .controller
            .add_account(
                draft(
                    Provider::Claude,
                    SignInMethod::Subscription,
                    "ChaoticQubit",
                    None,
                ),
                None,
            )
            .await
            .unwrap();
        assert_eq!(waiting.status, AccountStatus::NotSignedIn);
    }
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    settle_setup(&mut shell, &mut connection).await;
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "[ ] Add an account"), "{rows:#?}");
    for line in [
        "No account is ready for",
        "agents yet. Sign one in",
        "or check its key or URL.",
    ] {
        assert!(shows(&rows, line), "{line}: {rows:#?}");
    }
    assert!(shows(&rows, "Enter opens Settings"), "{rows:#?}");
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Settings(super::SettingsSection::Accounts))
    );
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));

    let saved = engine
        .controller
        .add_account(
            draft(
                Provider::AnthropicApi,
                SignInMethod::ApiKey,
                "Team key",
                Some("claude-sonnet-4-5"),
            ),
            Some(AccountSecret::new(b"unit-placeholder-anthropic-key-0001".to_vec()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(saved.status, AccountStatus::KeySaved);
    shell.refresh();
    pump(&mut shell, &mut connection).await;
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "[x] Add an account"), "{rows:#?}");
    assert!(shows(&rows, "Team key is ready for"), "{rows:#?}");
    assert!(shows(&rows, "agents."), "{rows:#?}");
    assert!(
        rows.iter().all(|row| row.chars().count() <= 80),
        "{rows:#?}"
    );
}

fn waiting_review() -> (Shell, ProjectId) {
    review_with(|_| {})
}

/// The onboarding page of `notes` after its onboarding read arrives: the graph
/// done and the mapping failed, changed by `change` first.
fn review_with(change: impl FnOnce(&mut OnboardingView)) -> (Shell, ProjectId) {
    let project = ProjectId::new("project-notes").unwrap();
    let mut ctx = ctx(80, 40);
    let screen = open(
        &Place::Onboarding {
            path: "/Users/owner/Projects/notes".into(),
            name: "notes".into(),
            project: Some(project.clone()),
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    while shell.ctx.next_request().is_some() {}
    let draft = OnboardingView {
        project_id: project.clone(),
        graph: Step::Done,
        mapping: Step::Failed {
            reason: "No model answered.".into(),
        },
        name: "notes".into(),
        draft: ProjectSetup {
            about: String::new(),
            tech_stack: vec!["rust".into()],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: None,
            documents: vec![],
        },
        facts: MappingFacts {
            languages: vec!["Rust".into()],
            build_commands: vec!["cargo build".into()],
            test_commands: vec!["cargo test".into()],
            entry_points: vec!["src/main.rs".into()],
        },
        missing: vec!["about".into(), "platforms".into()],
        overview: None,
        finished: false,
    };
    let mut draft = draft;
    change(&mut draft);
    shell.arrive(intake(
        IntakeQuery::Onboarding {
            project: project.clone(),
        },
        Ok(IntakeView::Onboarding(Box::new(draft))),
    ));
    (shell, project)
}

#[test]
fn a_graph_step_turned_off_in_setup_says_so_while_mapping_runs() {
    let (mut shell, _) = review_with(|view| {
        view.graph = Step::Off;
        view.mapping = Step::Running;
    });
    let rows = render(&mut shell, 80, 40);
    let off = format!("{} Turned off", shell.ctx.glyphs.status(Status::Off));
    assert!(shows(&rows, "Build the code graph"), "{rows:#?}");
    assert!(shows(&rows, &off), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "Graphify is off in Setup, so no code graph is built."
        ),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Running"), "{rows:#?}");
}

#[test]
fn the_review_marks_what_is_missing_and_clears_it_as_the_owner_fills_it() {
    let (mut shell, project) = waiting_review();
    let Some(Request::Setup(SetupQuery::Setup { project: read })) = shell.ctx.next_request() else {
        panic!("the review reads the setup revision");
    };
    assert_eq!(read, project);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Missing: About, Platforms."), "{rows:#?}");
    assert!(shows(&rows, "Languages: Rust"), "{rows:#?}");
    assert!(shows(&rows, "Tests: cargo test"), "{rows:#?}");

    shell.key(Key::Tab);
    keys(&mut shell, "Keeps notes in the terminal.");
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Missing: Platforms."), "{rows:#?}");
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "[x] CLI"), "{rows:#?}");
    assert!(shows(&rows, "Check the setup and confirm it."), "{rows:#?}");
}

#[test]
fn a_failed_mapping_shows_its_reason_and_r_on_confirm_maps_again() {
    let (mut shell, _) = review_with(|view| {
        view.draft.pool = Some(crate::protocol::CredentialRef::new("main").unwrap());
    });
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "Mapping failed: No model answered."),
        "{rows:#?}"
    );
    shell.key(Key::BackTab);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Retry mapping"), "{rows:#?}");
    shell.key(Key::Char('r'));
    let Some(Request::Intake(job)) = shell.ctx.next_request() else {
        panic!("r on Confirm maps the project again");
    };
    let IntakeJob::Query(IntakeQuery::StartOnboarding { path, pool, .. }) = *job else {
        panic!("a retry starts the onboarding again");
    };
    assert_eq!(path, "/Users/owner/Projects/notes");
    assert_eq!(pool.as_str(), "main");
}

#[test]
fn a_saved_setup_opens_with_its_saved_values_and_saves_changes() {
    let (mut shell, _) = review_with(|view| {
        view.finished = true;
        view.graph = Step::Running;
        view.mapping = Step::Waiting;
        view.draft.about = "Keeps notes in the terminal.".into();
        view.missing = vec!["platforms".into()];
    });
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "This project is set up."), "{rows:#?}");
    assert!(shows(&rows, "Keeps notes in the terminal."), "{rows:#?}");
    assert!(!shows(&rows, "Mapping failed"), "{rows:#?}");
    shell.key(Key::BackTab);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Save changes"), "{rows:#?}");
    assert!(!shows(&rows, "Retry mapping"), "{rows:#?}");
}

#[test]
fn confirming_the_review_sends_the_edited_setup_and_opens_the_project() {
    let (mut shell, project) = waiting_review();
    while shell.ctx.next_request().is_some() {}
    let stored = crate::project_setup::StoredSetup {
        project_id: project.clone(),
        name: "notes".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: "/Users/owner/Projects/notes".into(),
        created_ms: 0,
        revision: 7,
    };
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        SetupEvent {
            query: SetupQuery::Setup {
                project: project.clone(),
            },
            result: Ok(crate::project_setup::SetupView::Setup(Some(Box::new(
                stored,
            )))),
        },
    )))));
    shell.key(Key::Tab);
    keys(&mut shell, "Keeps notes.");
    for _ in 0..3 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    let Some(Request::Setup(SetupQuery::Change { action, generation })) = shell.ctx.next_request()
    else {
        panic!("confirm sends the setup change");
    };
    let SetupAction::Update {
        project_id,
        expected_revision,
        name,
        setup,
    } = action.clone()
    else {
        panic!("confirm updates the project setup");
    };
    assert_eq!(project_id, project);
    assert_eq!(expected_revision, 7);
    assert_eq!(name, None, "the name did not change");
    assert_eq!(setup.about, "Keeps notes.");
    assert_eq!(setup.tech_stack, ["rust"]);
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        SetupEvent {
            query: SetupQuery::Change { action, generation },
            result: Ok(crate::project_setup::SetupView::Changed(
                crate::engine::owner::OwnerResult::Applied,
            )),
        },
    )))));
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: project,
            name: "notes".into(),
            tab: ProjectTab::Overview,
        })
    );
}

#[test]
fn a_pasted_github_token_leaves_only_as_its_own_job_and_is_never_drawn() {
    let mut ctx = ctx(80, 24);
    let screen = open(&Place::CloneRepo, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let Some(request) = shell.ctx.next_request() else {
        panic!("the clone page reads the GitHub status");
    };
    assert_eq!(format!("{request:?}"), "Intake");
    shell.arrive(intake(
        IntakeQuery::GitHubStatus,
        Ok(IntakeView::GitHub(GitHubStatus {
            signed_in: false,
            login: None,
            method: GitHubMethod::None,
            waiting: false,
            problem: None,
        })),
    ));
    let rows = render(&mut shell, 80, 24);
    for way in ["Sign in with GitHub", "Paste a token", "Use the GitHub CLI"] {
        assert!(shows(&rows, way), "{way} missing: {rows:#?}");
    }
    shell.key(Key::Down);
    shell.key(Key::Enter);
    keys(&mut shell, "ghp_tuiSampleValue123");
    let rows = render(&mut shell, 80, 24);
    assert!(!shows(&rows, "ghp_"), "{rows:#?}");
    shell.key(Key::Enter);
    let Some(Request::Intake(job)) = shell.ctx.next_request() else {
        panic!("the token goes to the engine");
    };
    assert!(matches!(*job, IntakeJob::GitHubToken(_)));
    assert_eq!(job.query(), IntakeQuery::GitHubStatus);
    assert!(shell.ctx.next_request().is_none());
}
