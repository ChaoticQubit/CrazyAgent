use super::project_time::{clock, local_offset};
use super::settings_accounts::tests::{ctx, render};
use super::{Place, ProjectTab, open};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::github::{GitHubMethod, GitHubRepo, GitHubRepoPage, GitHubStatus};
use crate::onboarding::{FoundResearch, MappingFacts, MappingTry, OnboardingView, Step};
use crate::project_setup::{KeptFile, ProjectSetup, ResearchKind, SetupAction, SetupQuery};
use crate::projects_home::icloud::Progress;
use crate::projects_home::{AddMode, AddOverride, Measured};
use crate::protocol::ProjectId;
use crate::tui::client::Connection;
use crate::tui::client::intake::{IntakeEvent, IntakeJob, IntakeQuery, IntakeView};
use crate::tui::client::{OwnerEvent, SetupEvent};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::glyphs::Status;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::seam::{Arrival, PoolChange, Request, ShellRequest};
use crate::tui::shell::Shell;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    #[cfg(target_os = "macos")]
    _keychain: crate::credentials::test_keychain::TestKeychain,
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
        #[cfg(target_os = "macos")]
        let keychain = {
            let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
            keychain.serve(&crate::github::namespace(root.path()));
            keychain
        };
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
            #[cfg(target_os = "macos")]
            _keychain: keychain,
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
                Request::Questions(query)
                    if *query == crate::tui::client::questions::QuestionsQuery::Counts =>
                {
                    connection.questions(*query).unwrap();
                }
                Request::Learning(query) => {
                    sent.push("learning");
                    connection.learning(*query).unwrap();
                }
                other => panic!("intake pages never send {other:?}"),
            }
        }
        quiet = if moved || connection.busy() || crate::tui::test_home::connecting(&shell.ctx.link)
        {
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
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Move big anyway?"), "{rows:#?}");
    assert!(shows(&rows, "This folder has no git history"), "{rows:#?}");
    assert!(
        shows(&rows, "This is a rename on the same disk"),
        "{rows:#?}"
    );
    assert!(!shows(&rows, "This folder is very large"), "{rows:#?}");
    assert!(
        source.join("data.bin").exists(),
        "nothing moves before the answer"
    );
    shell.key(Key::Right);
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
async fn a_copy_of_a_repo_with_linked_worktrees_says_they_stay_with_the_original() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let outside = engine.user().join("outside");
    let source = outside.join("shop");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("README.md"), "shop\n").unwrap();
    let git = |args: &[&str]| {
        let done = crate::spawn_gate::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(args),
        )
        .unwrap();
        assert!(done.status.success(), "{done:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "README.md"]);
    git(&[
        "-c",
        "user.name=Owner",
        "-c",
        "user.email=owner@example.invalid",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    for branch in ["side", "other"] {
        let tree = outside.join(format!("shop-{branch}"));
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            tree.to_str().unwrap(),
        ]);
    }
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Size: "), "{rows:#?}");
    assert!(
        shows(&rows, "This folder has 2 linked git worktrees. They stay"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "the copy gets the main checkout and every branch."),
        "{rows:#?}"
    );

    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"add existing"), "{sent:?}");
    assert!(home.join("shop").join("README.md").is_file());
    assert!(!home.join("shop").join(".git").join("worktrees").exists());
    assert!(
        source
            .join(".git")
            .join("worktrees")
            .join("shop-side")
            .is_dir()
    );
}

fn git_in(folder: &Path, args: &[&str]) {
    let done = crate::spawn_gate::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(args),
    )
    .unwrap();
    assert!(done.status.success(), "{done:?}");
}

fn repo_with_worktree(source: &Path, linked: &Path) {
    std::fs::create_dir_all(source).unwrap();
    std::fs::write(source.join("README.md"), "shop\n").unwrap();
    git_in(source, &["init", "-q", "-b", "main"]);
    git_in(source, &["add", "README.md"]);
    git_in(
        source,
        &[
            "-c",
            "user.name=Owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    git_in(
        source,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().unwrap(),
        ],
    );
}

fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("no row shows {text:?}: {rows:#?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_of_a_repo_with_a_linked_worktree_asks_first_and_move_anyway_moves_it() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let outside = engine.user().join("outside");
    let source = outside.join("shop");
    let linked = outside.join("shop-side");
    repo_with_worktree(&source, &linked);
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "(*) Move"), "{rows:#?}");
    assert!(shows(&rows, "( ) Copy"), "{rows:#?}");
    assert!(!shows(&rows, "isn't offered"), "{rows:#?}");

    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Move shop anyway?"), "{rows:#?}");
    let loss = row_of(&rows, "Close editors, terminals and apps");
    let effect = row_of(&rows, "Git worktrees linked to this folder stop working");
    assert!(loss < effect, "{rows:#?}");
    assert!(
        shows(&rows, "Anything that points at the old place"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "This is a rename on the same disk"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, linked.canonicalize().unwrap().to_str().unwrap()),
        "{rows:#?}"
    );
    let buttons = row_of(&rows, "Move anyway");
    assert!(
        rows[buttons].find("Use where it is") < rows[buttons].find("Move anyway"),
        "{rows:#?}"
    );
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    assert!(source.join(".git").is_dir());

    shell.key(Key::Right);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"add existing"), "{sent:?}");
    assert!(home.join("shop").join(".git").is_dir());
    assert!(!source.exists());
    assert_eq!(
        shell.place(),
        Some(Place::Onboarding {
            path: home.join("shop").to_str().unwrap().into(),
            name: "shop".into(),
            project: None,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn use_where_it_is_in_the_question_sets_a_folder_without_git_up_in_place() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let source = engine.user().join("outside").join("notes");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("README.md"), "notes\n").unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Move notes anyway?"), "{rows:#?}");
    assert!(
        row_of(&rows, "Close editors, terminals and apps")
            < row_of(&rows, "This folder has no git history"),
        "{rows:#?}"
    );
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"add existing"), "{sent:?}");
    assert!(source.join("README.md").is_file());
    assert!(!home.join("notes").exists());
    assert_eq!(
        shell.place(),
        Some(Place::Onboarding {
            path: source.canonicalize().unwrap().to_str().unwrap().into(),
            name: "notes".into(),
            project: None,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_of_a_linked_worktree_asks_first_and_copy_anyway_copies_it() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let outside = engine.user().join("outside");
    let source = outside.join("shop");
    let linked = outside.join("shop-side");
    repo_with_worktree(&source, &linked);
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", linked.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "(*) Copy"), "{rows:#?}");
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Copy shop-side anyway?"), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "The copy keeps this folder's link to another repository's git data"
        ),
        "{rows:#?}"
    );
    assert!(
        !shows(&rows, "Close editors, terminals and apps"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Copy anyway"), "{rows:#?}");
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"add existing"), "{sent:?}");
    assert!(home.join("shop-side").join(".git").is_file());
    assert!(linked.join(".git").is_file());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_checkout_is_the_first_data_loss_line_of_a_copy_question() {
    let mut engine = Engine::start().await;
    engine.set_home().await;
    let outside = engine.user().join("outside");
    let source = outside.join("shop");
    let linked = outside.join("shop-side");
    repo_with_worktree(&source, &linked);
    std::fs::File::create(linked.join("data.bin"))
        .unwrap()
        .set_len(3 << 29)
        .unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", linked.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Copy shop-side anyway?"), "{rows:#?}");
    assert!(
        row_of(
            &rows,
            "The copy keeps this folder's link to another repository's git data"
        ) < row_of(&rows, "This folder is very large"),
        "{rows:#?}"
    );
    shell.key(Key::Esc);
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    assert!(!shows(&render(&mut shell, 80, 40), "anyway?"));
}

/// Opens the Move question for a folder `make` builds under the user's outside folder,
/// measured first through Copy so the question opens on the first Enter of Move.
/// Takes the builder. Returns the rendered rows.
async fn move_question(make: impl FnOnce(&Path) -> std::path::PathBuf) -> Vec<String> {
    let mut engine = Engine::start().await;
    engine.set_home().await;
    let outside = engine.user().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let folder = make(&outside);
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", folder.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["folder size"]);
    shell.key(Key::Left);
    shell.key(Key::Enter);
    render(&mut shell, 80, 40)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_of_a_linked_worktree_names_it_and_git_worktree_repair_in_it() {
    let rows = move_question(|outside| {
        let linked = outside.join("shop-side");
        repo_with_worktree(&outside.join("shop"), &linked);
        linked
    })
    .await;
    assert!(shows(&rows, "Move shop-side anyway?"), "{rows:#?}");
    assert!(
        shows(&rows, "This folder is a git worktree of another repository"),
        "{rows:#?}"
    );
    assert!(
        row_of(&rows, "This folder is a git worktree of another repository")
            < row_of(&rows, "Close editors, terminals and apps"),
        "a moved linked worktree can lose its staged changes, so it is the first loss line: {rows:#?}"
    );
    assert!(
        !shows(&rows, "Git worktrees linked to this folder"),
        "{rows:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_of_a_submodule_says_its_parent_no_longer_finds_it() {
    let rows = move_question(|outside| {
        let sub = outside.join("vendored");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(outside.join("shop/.git/modules/vendored")).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../shop/.git/modules/vendored\n").unwrap();
        sub
    })
    .await;
    assert!(
        shows(&rows, "This folder is a submodule of another repository"),
        "{rows:#?}"
    );
    assert!(
        !shows(&rows, "Git worktrees linked to this folder"),
        "{rows:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_of_a_folder_with_unreadable_git_data_says_it_cannot_tell() {
    let mut locked = None;
    let rows = move_question(|outside| {
        let folder = outside.join("guarded");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        std::fs::set_permissions(folder.join(".git"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        locked = Some(folder.join(".git"));
        folder
    })
    .await;
    let locked = locked.unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        shows(&rows, "CrazyAgent could not read this folder's git data"),
        "{rows:#?}"
    );
    assert!(
        !shows(&rows, "Git worktrees linked to this folder"),
        "{rows:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_reads_the_folder_size_first_and_warns_about_unreadable_files() {
    let mut engine = Engine::start().await;
    engine.set_home().await;
    let source = engine.user().join("outside").join("notes");
    std::fs::create_dir_all(source.join(".secret")).unwrap();
    std::fs::write(source.join("README.md"), "notes\n").unwrap();
    std::fs::set_permissions(
        source.join(".secret"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["home"]);
    keys(&mut shell, &format!("{}/", source.display()));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    std::fs::set_permissions(
        source.join(".secret"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(sent, ["folder size"], "a Move reads the size first");
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Move notes anyway?"), "{rows:#?}");
    assert!(shows(&rows, "Some files can't be read."), "{rows:#?}");
}

#[test]
fn a_soft_refusal_from_the_engine_turns_into_the_question_for_that_issue() {
    let user = tempfile::tempdir().unwrap();
    let source = user.path().canonicalize().unwrap().join("shop");
    std::fs::create_dir_all(source.join(".git")).unwrap();
    let mut ctx = screen_ctx(user.path());
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let typed = source.to_str().unwrap().to_owned();
    keys(&mut shell, &format!("{typed}/"));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    shell.arrive(intake(
        IntakeQuery::FolderSize {
            path: typed.clone(),
        },
        Ok(IntakeView::FolderSize(Measured {
            bytes: 10,
            placeholders: 0,
        })),
    ));
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    shell.arrive(intake(
        IntakeQuery::AddExisting {
            source: typed.clone(),
            mode: AddMode::Move,
            overrides: Vec::new(),
            generation: 1,
        },
        Err("home_copy_too_large".into()),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Move shop anyway?"), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "The folder is deeper or bigger than CrazyAgent checks"
        ),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "This copies every file to the other disk"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Move anyway"), "{rows:#?}");
    shell.key(Key::Right);
    shell.key(Key::Enter);
    shell.arrive(intake(
        IntakeQuery::AddExisting {
            source: typed,
            mode: AddMode::Move,
            overrides: vec![AddOverride::TooLarge],
            generation: 2,
        },
        Err("home_destination_exists".into()),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(!shows(&rows, "anyway?"), "{rows:#?}");
    assert!(shows(&rows, "already has a folder"), "{rows:#?}");
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

/// Serves the setup change and pool edit a confirmed onboarding review sends,
/// until the review opens its project. Takes the shell and its engine connection.
/// Returns the requests sent, in order.
async fn serve_until_project(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
        }
        if matches!(shell.place(), Some(Place::Project { .. })) {
            return sent;
        }
        if !connection.busy()
            && let Some(request) = shell.ctx.next_request()
        {
            match request {
                Request::Setup(query) => {
                    sent.push("setup");
                    connection.project_setup(query).unwrap();
                }
                Request::EditPool { change } => {
                    sent.push("edit pool");
                    let generation = shell.ctx.next_generation();
                    connection
                        .shell(ShellRequest::EditPool { change, generation })
                        .unwrap();
                }
                other => panic!("confirming a setup never sends {other:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "the confirmed setup never opened its project: {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirmed_setup_gives_the_chosen_pool_to_the_project_master() {
    let mut engine = Engine::start().await;
    let home = engine.set_home().await;
    let folder = home.join("notes");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join("Cargo.toml"),
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let path: String = folder.to_str().unwrap().into();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(
        &Place::Onboarding {
            path: path.clone(),
            name: "notes".into(),
            project: None,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["pools"]);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let Some(Place::Onboarding {
        project: Some(project),
        ..
    }) = shell.place()
    else {
        panic!("the started onboarding names its project");
    };
    drop(connection);
    drop(shell);

    let worker = crate::onboarding::service::start_for_engine(
        engine.root.path(),
        &engine.root.path().join("global.toml"),
        &engine.root.path().join("project.toml"),
        None,
        None,
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let onboarding = engine.controller.onboarding(project.clone()).await.unwrap();
        let done = |step: &Step| {
            matches!(
                step,
                Step::Done | Step::Off | Step::NotInstalled | Step::Failed { .. }
            )
        };
        if done(&onboarding.graph) && done(&onboarding.mapping) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the onboarding never settled: {onboarding:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    worker.stop();

    let mut ctx = screen_ctx(&engine.user());
    let screen = open(
        &Place::Onboarding {
            path,
            name: "notes".into(),
            project: Some(project.clone()),
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    pump(&mut shell, &mut connection).await;
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Confirm"), "{rows:#?}");
    assert!(
        shows(&rows, "[x] Plan a roadmap after research"),
        "an imported project plans a roadmap by default: {rows:#?}"
    );
    shell.key(Key::Tab);
    keys(&mut shell, "Keeps notes.");
    for _ in 0..4 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    let sent = serve_until_project(&mut shell, &mut connection).await;
    assert_eq!(sent, ["setup", "edit pool"], "{sent:?}");
    let stored = engine
        .controller
        .project_setup(project.clone())
        .await
        .unwrap()
        .expect("the confirmed setup is saved");
    assert!(
        stored
            .research
            .contains(&crate::project_setup::ResearchKind::RoadmapPlanning),
        "the confirmed setup keeps the toggle on"
    );

    let reply = engine
        .controller
        .read(crate::engine::owner::OwnerRead::ModelAccessPage {
            collection: crate::engine::owner::ModelAccessCollection::Pools,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let crate::engine::owner::OwnerData::ModelAccessPage { items, .. } = reply.data else {
        panic!("the engine answers a pool page");
    };
    let pool = items
        .iter()
        .find_map(|item| match item {
            crate::engine::owner::ModelAccessRecord::Pool { pool }
                if pool.id.as_str() == "pool-a" =>
            {
                Some(pool)
            }
            _ => None,
        })
        .expect("the chosen pool is still there");
    assert!(
        pool.projects
            .iter()
            .any(|scope| scope.as_str() == project.as_str()),
        "the chosen pool serves the set-up project: {pool:?}"
    );
    assert!(
        pool.roles
            .iter()
            .any(|role| role.as_str() == "project_leader"),
        "the chosen pool may lead the project as its master: {pool:?}"
    );
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
    let help = crate::tui::screens::settings_accounts::tests::bar_rows(&rows).join("\n");
    assert!(help.contains("Enter Install and continue"), "{rows:#?}");
    assert!(help.contains("c Continue without installing"), "{rows:#?}");

    shell.key(Key::Char('c'));
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
    shell.key(Key::Right);
    shell.key(Key::Enter);
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
                | crate::setup::DependencyState::Bundled { .. }
                | crate::setup::DependencyState::System { .. }
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

/// DEC-153: a fresh engine has every learning step open, so Home shows Learn CrazyAgent with
/// setup under Basics; h hides the guide through the engine's skipped mark and Home falls
/// back to Get set up while setup is not done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_reads_learn_crazyagent_from_the_engine_and_hide_guide_keeps_get_set_up() {
    let mut engine = Engine::start().await;
    let mut ctx = screen_ctx(&engine.user());
    ctx.store.learning = Load::Idle;
    let screen = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    settle_setup(&mut shell, &mut connection).await;
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Learn CrazyAgent"), "{rows:#?}");
    assert!(shows(&rows, " of 13 done"), "{rows:#?}");
    assert!(shows(&rows, "Basics"), "{rows:#?}");
    assert!(shows(&rows, "[ ] Add an account"), "{rows:#?}");
    assert!(shows(&rows, "[ ] Create a project"), "{rows:#?}");
    assert!(!shows(&rows, "Get set up"), "{rows:#?}");
    let footer = rows.last().unwrap();
    assert!(footer.contains("h Hide guide"), "{rows:#?}");
    let done = |rows: &[String]| -> usize {
        let row = rows
            .iter()
            .find(|row| row.contains(" of 13 done"))
            .expect("the progress line");
        let text = row.rsplit('│').next().unwrap().trim();
        text.split(' ').next().unwrap().parse().unwrap()
    };
    let before = done(&rows);

    shell.key(Key::Char('u'));
    assert_eq!(shell.place(), Some(Place::Usage));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(
        sent.contains(&"learning"),
        "the visit sends the metrics and usage marks: {sent:?}"
    );
    let engine_read = engine.controller.learning_progress().await.unwrap();
    for id in [
        crate::learning::StepId::Metrics,
        crate::learning::StepId::Usage,
    ] {
        assert!(
            engine_read
                .steps
                .iter()
                .any(|step| step.id == id && step.done()),
            "{id:?} ticks from the visit: {engine_read:?}"
        );
    }
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    pump(&mut shell, &mut connection).await;
    assert_eq!(done(&render(&mut shell, 80, 24)), before + 2);

    shell.key(Key::Char('h'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"learning"), "{sent:?}");
    pump(&mut shell, &mut connection).await;
    assert!(
        matches!(&shell.ctx.store.learning, Load::Ready(progress) if progress.skipped),
        "{:?}",
        shell.ctx.store.learning
    );
    assert!(
        engine.controller.learning_progress().await.unwrap().skipped,
        "the engine keeps the skip"
    );
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Get set up"), "{rows:#?}");
    assert!(!shows(&rows, "Learn CrazyAgent"), "{rows:#?}");
    assert!(shows(&rows, "[ ] Add an account"), "{rows:#?}");
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
        mapping_try: None,
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
            ..MappingFacts::default()
        },
        missing: vec!["about".into(), "platforms".into()],
        overview: None,
        folder_overview: None,
        finished: false,
        imported: Vec::new(),
        proposed: None,
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
fn a_running_mapping_shows_its_try_its_minutes_and_why_the_earlier_try_failed() {
    let now = crate::tui::clock::now_ms();
    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::Running;
        view.mapping_try = Some(MappingTry {
            number: 2,
            tries: 3,
            started_ms: now - 150_000,
            earlier: Some("Mapping agent ran out of time".into()),
        });
    });
    let rows = render(&mut shell, 100, 40);
    assert!(
        shows(
            &rows,
            "Mapping, try 2 of 3, 2 min. Try 1 failed: Mapping agent ran out of time"
        ),
        "{rows:#?}"
    );

    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::Running;
        view.mapping_try = Some(MappingTry {
            number: 1,
            tries: 3,
            started_ms: now + 5_000,
            earlier: None,
        });
    });
    let rows = render(&mut shell, 100, 40);
    assert!(
        shows(&rows, "Mapping, try 1 of 3, under 1 min."),
        "{rows:#?}"
    );
    assert!(!shows(&rows, "failed:"), "{rows:#?}");

    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::WAITING;
        view.mapping_try = Some(MappingTry {
            number: 2,
            tries: 3,
            started_ms: now,
            earlier: Some("Mapping agent ran out of time".into()),
        });
    });
    let rows = render(&mut shell, 100, 40);
    assert!(!shows(&rows, "Mapping, try"), "{rows:#?}");
}

#[test]
fn an_engine_reason_with_hidden_or_direction_changing_characters_shows_them_as_escapes() {
    const HIDDEN: &str = "\u{202e}exe\u{200b}";
    const ESCAPED: &str = "\\u202eexe\\u200b";
    let mut raw = Vec::new();

    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::Running;
        view.mapping_try = Some(MappingTry {
            number: 2,
            tries: 3,
            started_ms: crate::tui::clock::now_ms(),
            earlier: Some(HIDDEN.into()),
        });
    });
    let rows = render(&mut shell, 100, 40);
    if !shows(&rows, &format!("Try 1 failed: {ESCAPED}")) {
        raw.push(("the running try's earlier reason", rows));
    }

    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::Failed {
            reason: format!("No model: {HIDDEN}"),
        };
    });
    let rows = render(&mut shell, 80, 40);
    if !shows(&rows, &format!("Mapping failed: No model: {ESCAPED}")) {
        raw.push(("the review's mapping failure", rows));
    }

    let (mut shell, _) = review_with(|view| {
        view.graph = Step::Failed {
            reason: format!("graphify stopped: {HIDDEN}"),
        };
    });
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    if !shows(&rows, &format!("graphify stopped: {ESCAPED}")) {
        raw.push(("a failed step's reason", rows));
    }

    assert!(raw.is_empty(), "shown without escapes: {raw:#?}");
}

#[test]
fn the_review_shows_the_roadmap_research_and_documents_already_in_the_repo() {
    let (mut shell, _) = review_with(|view| {
        view.facts.roadmap = vec!["research/docs/ss-phase1-revision/report.md".into()];
        view.facts.research = vec![FoundResearch {
            kind: ResearchKind::CompetitorResearch,
            files: vec!["research/landscape-2026.md".into()],
        }];
        view.facts.documents = vec!["docs/q3-plan.md".into()];
    });
    let rows = render(&mut shell, 100, 80);
    assert!(
        shows(
            &rows,
            "Roadmap found: research/docs/ss-phase1-revision/report.md"
        ),
        "{rows:#?}"
    );
    assert!(
        shows(
            &rows,
            "Competitor research - Linked: research/landscape-2026.md"
        ),
        "{rows:#?}"
    );
    assert!(
        shows(
            &rows,
            "CrazyAgent reads them from your repo; edits go to those files."
        ),
        "{rows:#?}"
    );
    assert!(shows(&rows, "[x] Use these files"), "{rows:#?}");
    assert!(
        !shows(&rows, "[x] Plan a roadmap after research"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Project documents found:"), "{rows:#?}");
    assert!(shows(&rows, "- docs/q3-plan.md"), "{rows:#?}");
    let (mut plain, _) = review_with(|_| {});
    let rows = render(&mut plain, 100, 80);
    assert!(!shows(&rows, "Linked:"), "{rows:#?}");
    assert!(!shows(&rows, "Roadmap found"), "{rows:#?}");
    assert!(!shows(&rows, "Use these files"), "{rows:#?}");
    assert!(!shows(&rows, "Project documents found"), "{rows:#?}");
}

#[test]
fn confirming_links_the_found_files_unless_the_owner_turns_them_off() {
    const ROADMAP: &str = "research/docs/ss-phase1-revision/report.md";
    let sent = |turn_off: bool| {
        let (mut shell, project) = review_with(|view| {
            view.facts.roadmap = vec![ROADMAP.into()];
            view.facts.research = vec![FoundResearch {
                kind: ResearchKind::CompetitorResearch,
                files: vec!["docs/rivals.md".into(), ROADMAP.into()],
            }];
        });
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
                query: SetupQuery::Setup { project },
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
        if turn_off {
            shell.key(Key::Char(' '));
        }
        shell.key(Key::Tab);
        shell.key(Key::Enter);
        let Some(Request::Setup(SetupQuery::Change { action, .. })) =
            shell.ctx.next_screen_request()
        else {
            panic!("confirm sends the setup change");
        };
        let SetupAction::Update { setup, kept, .. } = action else {
            panic!("confirm updates the project setup");
        };
        assert!(
            !setup.research.contains(&ResearchKind::RoadmapPlanning),
            "a found roadmap starts with roadmap planning off"
        );
        kept
    };
    let file = |path: &str, research: Option<ResearchKind>| KeptFile {
        path: path.to_owned(),
        research,
    };
    assert_eq!(
        sent(false),
        Some(vec![
            file(ROADMAP, None),
            file("docs/rivals.md", Some(ResearchKind::CompetitorResearch)),
            file(ROADMAP, Some(ResearchKind::CompetitorResearch)),
        ]),
        "each found file goes with the group it was found under"
    );
    assert_eq!(sent(true), Some(Vec::new()));
}

#[test]
fn planning_a_roadmap_is_off_and_skipped_while_the_found_roadmap_is_linked() {
    const ROADMAP: &str = "docs/roadmap.md";
    const PLAN: &str = "[x] Plan a roadmap after research";
    const NOTE: &str = "Not planned while the found roadmap is linked.";
    let roadmap_file = || KeptFile {
        path: ROADMAP.to_owned(),
        research: None,
    };
    let sent = |shell: &mut Shell| {
        shell.key(Key::Enter);
        let Some(Request::Setup(SetupQuery::Change { action, .. })) =
            shell.ctx.next_screen_request()
        else {
            panic!("confirm sends the setup change");
        };
        let SetupAction::Update { setup, kept, .. } = action else {
            panic!("confirm updates the project setup");
        };
        (
            setup.research.contains(&ResearchKind::RoadmapPlanning),
            kept,
        )
    };
    let opened = |planned: bool| {
        let (mut shell, project) = review_with(|view| {
            view.facts.roadmap = vec![ROADMAP.into()];
            if planned {
                view.draft.research = vec![ResearchKind::RoadmapPlanning];
            }
        });
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
                query: SetupQuery::Setup { project },
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
        shell
    };

    let mut linked = opened(true);
    let rows = render(&mut linked, 100, 80);
    assert!(!shows(&rows, PLAN), "{rows:#?}");
    assert!(shows(&rows, NOTE), "{rows:#?}");
    linked.key(Key::BackTab);
    linked.key(Key::Char(' '));
    linked.key(Key::Tab);
    linked.key(Key::Tab);
    assert_eq!(
        sent(&mut linked),
        (false, Some(vec![roadmap_file()])),
        "the toggle is skipped, so no planning is asked for beside the linked roadmap"
    );

    let mut unlinked = opened(false);
    unlinked.key(Key::Char(' '));
    unlinked.key(Key::BackTab);
    unlinked.key(Key::Char(' '));
    let rows = render(&mut unlinked, 100, 80);
    assert!(shows(&rows, PLAN), "{rows:#?}");
    assert!(!shows(&rows, NOTE), "{rows:#?}");
    unlinked.key(Key::Tab);
    unlinked.key(Key::Tab);
    assert_eq!(
        sent(&mut unlinked),
        (true, Some(Vec::new())),
        "with the files turned off the owner can ask for a planned roadmap"
    );

    let mut again = opened(false);
    again.key(Key::Char(' '));
    again.key(Key::BackTab);
    again.key(Key::Char(' '));
    again.key(Key::Tab);
    again.key(Key::Char(' '));
    let rows = render(&mut again, 100, 80);
    assert!(!shows(&rows, PLAN), "{rows:#?}");
    assert!(shows(&rows, NOTE), "{rows:#?}");
    again.key(Key::Tab);
    assert_eq!(
        sent(&mut again),
        (false, Some(vec![roadmap_file()])),
        "linking the files again takes the planning back off"
    );
}

#[test]
fn mapping_paused_on_every_pool_account_shows_when_the_pause_ends() {
    const UNTIL_MS: i64 = 1_792_729_920_000;
    let (mut shell, _) = review_with(|view| {
        view.mapping = Step::Waiting {
            until_ms: Some(UNTIL_MS),
        };
    });
    let rows = render(&mut shell, 80, 40);
    let end = clock(UNTIL_MS, local_offset(UNTIL_MS));
    let paused = format!(
        "{} Rate limited until {end}",
        shell.ctx.glyphs.status(Status::Needs)
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Map the project") && row.contains(&paused)),
        "{rows:#?}"
    );

    let (mut shell, _) = review_with(|view| view.mapping = Step::WAITING);
    let rows = render(&mut shell, 80, 40);
    assert!(
        rows.iter()
            .any(|row| row.contains("Map the project") && row.contains("Waiting")),
        "{rows:#?}"
    );
    assert!(!shows(&rows, "Rate limited"), "{rows:#?}");
}

#[test]
fn the_review_still_says_whether_a_code_graph_was_built() {
    let graph_row = |rows: &[String], state: &str| {
        rows.iter()
            .any(|row| row.contains("Build the code graph") && row.contains(state))
    };
    let (mut shell, _) = review_with(|view| view.graph = Step::Off);
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    assert!(graph_row(&rows, "Turned off"), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "Graphify is off in Setup, so no code graph is built."
        ),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "Mapping failed: No model answered."),
        "{rows:#?}"
    );

    let (mut shell, _) = review_with(|view| {
        view.graph = Step::Failed {
            reason: "graphify not installed".into(),
        }
    });
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    assert!(graph_row(&rows, "Failed"), "{rows:#?}");
    assert!(shows(&rows, "graphify not installed"), "{rows:#?}");

    let (mut shell, _) = waiting_review();
    while shell.ctx.next_request().is_some() {}
    let rows = render(&mut shell, 80, 40);
    assert!(graph_row(&rows, "Done"), "{rows:#?}");
}

#[test]
fn a_graph_step_without_the_graphify_skill_says_it_is_not_installed_not_turned_off() {
    let (mut shell, _) = review_with(|view| {
        view.graph = Step::NotInstalled;
        view.mapping = Step::Running;
    });
    let rows = render(&mut shell, 80, 40);
    let missing = format!("{} Not installed", shell.ctx.glyphs.status(Status::Off));
    assert!(shows(&rows, &missing), "{rows:#?}");
    assert!(!shows(&rows, "Turned off"), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "Graphify isn't installed, so no code graph is built. Install it in Setup."
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
        view.mapping = Step::WAITING;
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

/// The review of `notes` with the pool `main` in its draft, confirmed and saved
/// by the engine. Returns the shell, the project and the pool change the page
/// sends next, if any.
fn confirmed_with_pool() -> (Shell, ProjectId, Option<Request>) {
    let pool = crate::protocol::CredentialRef::new("main").unwrap();
    let (mut shell, project) = review_with(|view| view.draft.pool = Some(pool.clone()));
    while shell.ctx.next_request().is_some() {}
    let stored = crate::project_setup::StoredSetup {
        project_id: project.clone(),
        name: "notes".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: Some(pool),
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
    for _ in 0..4 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    let Some(Request::Setup(SetupQuery::Change { action, generation })) =
        shell.ctx.next_screen_request()
    else {
        panic!("confirm sends the setup change");
    };
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        SetupEvent {
            query: SetupQuery::Change { action, generation },
            result: Ok(crate::project_setup::SetupView::Changed(
                crate::engine::owner::OwnerResult::Applied,
            )),
        },
    )))));
    let next = shell.ctx.next_request();
    (shell, project, next)
}

#[test]
fn a_confirmed_setup_gives_its_pool_to_the_project_before_the_project_opens() {
    let opened = |project: &ProjectId| Place::Project {
        id: project.clone(),
        name: "notes".into(),
        tab: ProjectTab::Overview,
    };
    let (mut shell, project, next) = confirmed_with_pool();
    let Some(Request::EditPool {
        change: PoolChange::Assign { pool, project: to },
    }) = next
    else {
        panic!("the saved setup's pool is given to the project: {next:?}");
    };
    assert_eq!(pool.as_ref().map(|id| id.as_str()), Some("main"));
    assert_eq!(to, project);
    assert_ne!(shell.place(), Some(opened(&project)));
    shell.arrive(Arrival::PoolEdited(Ok(())));
    assert_eq!(shell.place(), Some(opened(&project)));
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "notes is ready."), "{rows:#?}");

    let (mut shell, project, _) = confirmed_with_pool();
    shell.arrive(Arrival::PoolEdited(Err("pool_assignment_rejected".into())));
    assert_eq!(shell.place(), Some(opened(&project)));
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "notes is set up, but its pool is not assigned."),
        "{rows:#?}"
    );
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
    for _ in 0..4 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    let Some(Request::Setup(SetupQuery::Change { action, generation })) =
        shell.ctx.next_screen_request()
    else {
        panic!("confirm sends the setup change");
    };
    let SetupAction::Update {
        project_id,
        expected_revision,
        name,
        setup,
        kept,
    } = action.clone()
    else {
        panic!("confirm updates the project setup");
    };
    assert_eq!(
        kept, None,
        "nothing was found, so the links stay as they are"
    );
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
    let Some(request) = shell.ctx.next_screen_request() else {
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
    let Some(Request::Intake(job)) = shell.ctx.next_screen_request() else {
        panic!("the token goes to the engine");
    };
    assert!(matches!(*job, IntakeJob::GitHubToken(_)));
    assert_eq!(job.query(), IntakeQuery::GitHubStatus);
    assert!(shell.ctx.next_screen_request().is_none());
}

#[test]
fn a_repository_the_engine_cannot_clone_is_listed_with_why_and_enter_never_clones_it() {
    let mut ctx = ctx(100, 24);
    let screen = open(&Place::CloneRepo, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    assert!(shell.ctx.next_screen_request().is_some());
    shell.arrive(intake(
        IntakeQuery::GitHubStatus,
        Ok(IntakeView::GitHub(GitHubStatus {
            signed_in: true,
            login: Some("octo-cat".into()),
            method: GitHubMethod::Token,
            waiting: false,
            problem: None,
        })),
    ));
    let Some(Request::Intake(job)) = shell.ctx.next_screen_request() else {
        panic!("a signed-in clone page lists the repositories");
    };
    assert_eq!(job.query(), IntakeQuery::GitHubRepos { page: 1 });
    let repo = |full_name: &str, refused: Option<&str>| GitHubRepo {
        full_name: full_name.into(),
        private: false,
        default_branch: "main".into(),
        size_kb: 4,
        refused: refused.map(Into::into),
    };
    shell.arrive(intake(
        IntakeQuery::GitHubRepos { page: 1 },
        Ok(IntakeView::Repos(GitHubRepoPage {
            repos: vec![
                repo("octo-cat/.github", Some("github_repo_name_unsupported")),
                repo("octo-cat/notes", None),
            ],
            next: None,
        })),
    ));
    let rows = render(&mut shell, 100, 24);
    assert!(shows(&rows, "octo-cat/.github"), "{rows:#?}");
    assert!(shows(&rows, "Can't clone this name"), "{rows:#?}");
    assert!(shows(&rows, "octo-cat/notes"), "{rows:#?}");
    shell.key(Key::Enter);
    assert!(shell.ctx.next_screen_request().is_none());
    let rows = render(&mut shell, 100, 24);
    assert!(
        shows(
            &rows,
            "can't clone a repository whose name starts with a dot"
        ),
        "{rows:#?}"
    );
    assert!(!shows(&rows, "github_repo_name_unsupported"), "{rows:#?}");
    shell.key(Key::Down);
    shell.key(Key::Enter);
    let Some(Request::Intake(job)) = shell.ctx.next_screen_request() else {
        panic!("the other repository still clones");
    };
    assert_eq!(
        job.query(),
        IntakeQuery::CloneRepo {
            full_name: "octo-cat/notes".into()
        }
    );
}

/// Starts an engine whose GitHub settings file sets an empty client id, so the engine
/// refuses a GitHub sign-in with `github_client_id_not_configured` before any network call.
async fn engine_without_github_client_id() -> Engine {
    let engine = Engine::start().await;
    let file = engine.root.path().join("github.toml");
    std::fs::write(&file, "client_id = \"\"\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    engine
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_github_sign_in_the_engine_refuses_says_why_on_the_clone_page() {
    let engine = engine_without_github_client_id().await;
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::CloneRepo, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    assert_eq!(pump(&mut shell, &mut connection).await, ["github"]);
    assert!(shows(&render(&mut shell, 100, 24), "Sign in with GitHub"));
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["github"]);
    let rows = render(&mut shell, 100, 24);
    assert!(
        shows(&rows, "Sign in with GitHub isn't set up in this build yet."),
        "{rows:#?}"
    );
    assert!(!shows(&rows, "Something went wrong"), "{rows:#?}");
    assert!(
        !shows(&rows, "github_client_id_not_configured"),
        "{rows:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_projects_home_the_engine_refuses_says_why_on_the_setup_page() {
    let engine = Engine::start().await;
    let file = engine.user().join("notes.txt");
    std::fs::write(&file, "not a folder").unwrap();
    let mut ctx = screen_ctx(&engine.user());
    let screen = open(&Place::Setup { first_run: false }, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let mut connection = engine.connect();
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('h'));
    keys(&mut shell, file.to_str().unwrap());
    shell.key(Key::Enter);
    assert_eq!(pump(&mut shell, &mut connection).await, ["settings"]);
    let message = shell.ctx.message().expect("the refusal is shown");
    assert_eq!(message.lead, "Pick a folder, not a file or a link.");
}

/// A real checkout with CLAUDE.md tracked and AGENTS.md untracked, imported by the
/// engine's own import, and the onboarding review of `notes` showing what it moved.
/// Output: the shell, the project, the folder (kept alive by its guard) and the tip
/// before the import.
fn imported_review() -> (
    Shell,
    ProjectId,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
) {
    let guard = tempfile::tempdir().unwrap();
    let folder = guard.path().canonicalize().unwrap();
    let tip = crate::tui::test_home::instruction_repo(&folder, &["CLAUDE.md"], &["AGENTS.md"]);
    let moved = crate::instruction_files::import(&folder).unwrap();
    assert_eq!(
        moved
            .iter()
            .map(|file| (file.path.as_str(), file.uncommitted))
            .collect::<Vec<_>>(),
        [("CLAUDE.md", true), ("AGENTS.md", false)]
    );
    let (mut shell, project) = review_with(|view| view.imported = moved);
    while shell.ctx.next_request().is_some() {}
    (shell, project, guard, folder, tip)
}

fn commit_reply(project: &ProjectId, folder: &Path, home: &Path) -> Arrival {
    let query = crate::tui::client::process::ProcessQuery::CommitImport {
        project: project.clone(),
    };
    let result = crate::instruction_files::commit(folder, home)
        .map(crate::tui::client::process::ProcessReply::Committed)
        .map_err(|problem| problem.0);
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(
        crate::tui::client::process::ProcessEvent { query, result },
    ))))
}

fn commit_sent(shell: &mut Shell, project: &ProjectId) -> bool {
    let sent: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    matches!(
        sent.as_slice(),
        [Request::Process(query)] if matches!(
            query.as_ref(),
            crate::tui::client::process::ProcessQuery::CommitImport { project: asked } if asked == project
        )
    )
}

#[test]
fn imported_files_are_listed_after_folder_import_and_ctrl_g_commits_the_move() {
    let (mut shell, project, _guard, folder, tip) = imported_review();
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "CrazyAgent copied these instruction files into"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "- CLAUDE.md (git shows it as deleted)"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim_end().ends_with("- AGENTS.md")),
        "an untracked file is not shown deleted: {rows:#?}"
    );
    assert!(
        shows(
            &rows,
            "Merges into this branch are refused until the move is"
        ),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Ctrl-G Commit the move"), "{rows:#?}");
    assert!(shows(&rows, "Ctrl-L Leave it"), "{rows:#?}");

    shell.key(Key::Ctrl('g'));
    assert!(commit_sent(&mut shell, &project));
    shell.key(Key::Ctrl('g'));
    assert!(shell.ctx.next_request().is_none(), "one commit at a time");
    let home = tempfile::tempdir().unwrap();
    shell.arrive(commit_reply(&project, &folder, home.path()));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Committed the move on main:");
    assert_eq!(message.rest, "CLAUDE.md. Nothing was pushed.");
    let rows = render(&mut shell, 80, 40);
    assert!(!shows(&rows, "(git shows it as deleted)"), "{rows:#?}");
    assert!(!shows(&rows, "Ctrl-G Commit the move"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row.trim_end().ends_with("- CLAUDE.md")),
        "{rows:#?}"
    );
    let git = |args: &[&str]| crate::tui::test_home::git(&folder, args);
    assert_eq!(git(&["rev-parse", "HEAD~1"]).trim(), tip);
    assert_eq!(
        git(&["show", "--name-status", "--format=", "HEAD"]).trim(),
        "D\tCLAUDE.md"
    );
    assert!(
        crate::instruction_files::uncommitted(&folder)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ctrl_l_leaves_the_move_and_hides_the_offer() {
    let (mut shell, _project, _guard, folder, tip) = imported_review();
    shell.key(Key::Ctrl('l'));
    assert!(shell.ctx.next_request().is_none(), "Leave it sends nothing");
    let rows = render(&mut shell, 80, 40);
    assert!(!shows(&rows, "Ctrl-G Commit the move"), "{rows:#?}");
    assert!(
        !shows(&rows, "Merges into this branch are refused"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "- CLAUDE.md (git shows it as deleted)"),
        "the file is still named as git shows it: {rows:#?}"
    );
    shell.key(Key::Ctrl('g'));
    assert!(
        shell.ctx.next_request().is_none(),
        "Ctrl-G no longer commits once left"
    );
    assert_eq!(
        crate::tui::test_home::git(&folder, &["rev-parse", "HEAD"]).trim(),
        tip
    );
}

#[test]
fn a_refused_onboarding_commit_says_why_and_keeps_the_offer() {
    let (mut shell, project, _guard, folder, tip) = imported_review();
    std::fs::write(folder.join("other.txt"), "staged\n").unwrap();
    crate::tui::test_home::git(&folder, &["add", "other.txt"]);
    shell.key(Key::Ctrl('g'));
    assert!(commit_sent(&mut shell, &project));
    let home = tempfile::tempdir().unwrap();
    shell.arrive(commit_reply(&project, &folder, home.path()));
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        super::process::message("process_commit_staged")
    );
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "- CLAUDE.md (git shows it as deleted)"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "Ctrl-G Commit the move"), "{rows:#?}");
    assert_eq!(
        crate::tui::test_home::git(&folder, &["rev-parse", "HEAD"]).trim(),
        tip
    );
}

/// Opens the Existing folder page on ~/Desktop/shop, a git folder, picks `mode`, and answers the
/// size read with `placeholders` files only in iCloud. Takes the user folder, the way and the
/// count. Returns the shell, after the Enter that goes on from the size, and the typed folder.
fn measured_icloud_folder(user: &Path, mode: AddMode, placeholders: u64) -> (Shell, String) {
    let user = user.canonicalize().unwrap();
    let source = user.join("Desktop").join("shop");
    std::fs::create_dir_all(source.join(".git")).unwrap();
    let mut ctx = screen_ctx(&user);
    let screen = open(&Place::AddFolder, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let typed = source.to_str().unwrap().to_owned();
    keys(&mut shell, &format!("{typed}/"));
    shell.key(Key::Tab);
    shell.key(Key::Right);
    if mode == AddMode::Copy {
        shell.key(Key::Right);
    }
    shell.key(Key::Enter);
    shell.arrive(intake(
        IntakeQuery::FolderSize {
            path: typed.clone(),
        },
        Ok(IntakeView::FolderSize(Measured {
            bytes: 10,
            placeholders,
        })),
    ));
    shell.key(Key::Enter);
    (shell, typed)
}

/// Takes every intake job the screen queued. Takes the shell. Returns the jobs in order.
fn intake_jobs(shell: &mut Shell) -> Vec<IntakeJob> {
    let mut jobs = Vec::new();
    while let Some(request) = shell.ctx.next_screen_request() {
        if let Request::Intake(job) = request {
            jobs.push(*job);
        }
    }
    jobs
}

/// Takes the one iCloud download the screen queued. Takes the shell. Returns its folder and
/// the flag that stops it.
fn download_job(shell: &mut Shell) -> (String, Arc<AtomicBool>) {
    let downloads: Vec<(String, Arc<AtomicBool>)> = intake_jobs(shell)
        .into_iter()
        .filter_map(|job| match job {
            IntakeJob::IcloudDownload { path, cancel } => Some((path, cancel)),
            _ => None,
        })
        .collect();
    assert_eq!(downloads.len(), 1, "one download is queued");
    downloads.into_iter().next().unwrap()
}

/// The ways of the adds the screen queued. Takes the shell and the folder. Returns them.
fn added_ways(shell: &mut Shell, folder: &str) -> Vec<AddMode> {
    intake_jobs(shell)
        .into_iter()
        .filter_map(|job| match job.query() {
            IntakeQuery::AddExisting { source, mode, .. } if source == folder => Some(mode),
            _ => None,
        })
        .collect()
}

fn icloud_arrival(folder: &str, result: Result<IntakeView, String>) -> Arrival {
    intake(
        IntakeQuery::IcloudDownload {
            path: folder.to_owned(),
        },
        result,
    )
}

#[test]
fn an_icloud_folder_asks_first_and_download_and_move_moves_once_every_file_is_here() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Move, 3);
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "Download shop from iCloud first?"),
        "{rows:#?}"
    );
    assert!(
        shows(&rows, "3 files are only in iCloud; CrazyAgent"),
        "{rows:#?}"
    );
    assert!(shows(&rows, "can take a while)."), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "Moving takes the folder out of iCloud Desktop on your"
        ),
        "{rows:#?}"
    );
    for choice in ["Download and move", "Download and copy", "Cancel"] {
        assert!(shows(&rows, choice), "{choice} missing: {rows:#?}");
    }
    assert!(added_ways(&mut shell, &typed).is_empty());
    shell.key(Key::Enter);
    let (path, cancel) = download_job(&mut shell);
    assert_eq!(path, typed);
    assert!(!cancel.load(Ordering::Relaxed));
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Downloading from iCloud"), "{rows:#?}");
    shell.arrive(icloud_arrival(
        &typed,
        Ok(IntakeView::IcloudProgress(Progress { total: 3, done: 1 })),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "Downloading from iCloud: 1 of 3 files"),
        "{rows:#?}"
    );
    shell.key(Key::Enter);
    assert!(
        intake_jobs(&mut shell).is_empty(),
        "keys wait while it downloads"
    );
    shell.arrive(icloud_arrival(&typed, Ok(IntakeView::IcloudDownloaded(3))));
    let rows = render(&mut shell, 80, 40);
    assert!(!shows(&rows, "from iCloud first?"), "{rows:#?}");
    assert!(shows(&rows, "Press Enter again to move it"), "{rows:#?}");
    shell.key(Key::Enter);
    assert_eq!(added_ways(&mut shell, &typed), [AddMode::Move]);
}

#[test]
fn download_and_copy_turns_a_move_into_a_copy_after_the_download() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Move, 1);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "1 file is only in iCloud"), "{rows:#?}");
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let (path, _) = download_job(&mut shell);
    assert_eq!(path, typed);
    shell.arrive(icloud_arrival(&typed, Ok(IntakeView::IcloudDownloaded(1))));
    assert_eq!(added_ways(&mut shell, &typed), [AddMode::Copy]);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Copying the folder"), "{rows:#?}");
}

#[test]
fn cancel_closes_the_icloud_question_and_downloads_or_adds_nothing() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Copy, 4);
    shell.key(Key::Right);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Enter Cancel"), "{rows:#?}");
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 40);
    assert!(!shows(&rows, "from iCloud first?"), "{rows:#?}");
    assert!(
        intake_jobs(&mut shell)
            .iter()
            .all(|job| !matches!(job, IntakeJob::IcloudDownload { .. })),
        "Cancel starts no download"
    );
    assert!(added_ways(&mut shell, &typed).is_empty());
}

#[test]
fn esc_stops_a_running_download_and_the_folder_is_not_added() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Move, 5);
    shell.key(Key::Enter);
    let (_, cancel) = download_job(&mut shell);
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Esc Stop the download"), "{rows:#?}");
    shell.key(Key::Esc);
    assert!(cancel.load(Ordering::Relaxed), "Esc sets the stop flag");
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Stopping the download"), "{rows:#?}");
    shell.arrive(icloud_arrival(
        &typed,
        Err("home_icloud_not_downloaded".into()),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "The download stopped. Nothing was moved or copied."),
        "{rows:#?}"
    );
    assert!(added_ways(&mut shell, &typed).is_empty());
}

#[test]
fn esc_just_before_a_download_ends_still_adds_nothing() {
    let user = tempfile::tempdir().unwrap();
    for mode in [AddMode::Copy, AddMode::Move] {
        let (mut shell, typed) = measured_icloud_folder(user.path(), mode, 2);
        shell.key(Key::Enter);
        let (_, cancel) = download_job(&mut shell);
        shell.key(Key::Esc);
        assert!(cancel.load(Ordering::Relaxed), "Esc sets the stop flag");
        shell.arrive(icloud_arrival(&typed, Ok(IntakeView::IcloudDownloaded(2))));
        let rows = render(&mut shell, 80, 40);
        assert!(
            shows(&rows, "The download stopped. Nothing was moved or copied."),
            "{rows:#?}"
        );
        assert!(!shows(&rows, "Copying the folder"), "{rows:#?}");
        assert!(!shows(&rows, "Press Enter again to move it"), "{rows:#?}");
        assert!(added_ways(&mut shell, &typed).is_empty(), "{mode:?}");
    }
}

#[test]
fn a_download_the_engine_ends_shows_its_sentence_and_adds_nothing() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Copy, 2);
    shell.key(Key::Enter);
    let _ = download_job(&mut shell);
    shell.arrive(icloud_arrival(
        &typed,
        Err("home_icloud_not_downloaded".into()),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(
        shows(&rows, "Some files are still only in iCloud"),
        "{rows:#?}"
    );
    assert!(added_ways(&mut shell, &typed).is_empty());
}

#[test]
fn an_add_the_engine_refuses_for_icloud_files_asks_to_download_them() {
    let user = tempfile::tempdir().unwrap();
    let (mut shell, typed) = measured_icloud_folder(user.path(), AddMode::Copy, 0);
    assert_eq!(added_ways(&mut shell, &typed), [AddMode::Copy]);
    shell.arrive(intake(
        IntakeQuery::AddExisting {
            source: typed.clone(),
            mode: AddMode::Copy,
            overrides: Vec::new(),
            generation: 1,
        },
        Err("home_icloud_not_downloaded".into()),
    ));
    let rows = render(&mut shell, 80, 40);
    assert!(shows(&rows, "Some files are only in iCloud"), "{rows:#?}");
    assert!(shows(&rows, "Enter Download and copy"), "{rows:#?}");
    shell.key(Key::Enter);
    let (path, _) = download_job(&mut shell);
    assert_eq!(path, typed);
    shell.arrive(icloud_arrival(&typed, Ok(IntakeView::IcloudDownloaded(9))));
    assert_eq!(added_ways(&mut shell, &typed), [AddMode::Copy]);
}
