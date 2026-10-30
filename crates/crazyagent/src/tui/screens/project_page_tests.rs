use super::new_project::{HarnessChoice, HarnessState};
use super::new_project_frames::check_frames;
use super::new_project_input::cells;
use super::new_project_page::{harness_choices_from, kept_draft, key_event, pool_choices_from};
use super::project::OpenRequest;
use super::project_page::{Opening, opening};
use super::{Flow, Place, ProjectTab, SettingsSection, open};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerMutation;
use crate::engine::owner::{OwnerClientKind, OwnerData, OwnerRead, OwnerResponse};
use crate::master_session::{
    MasterOwnerAction, MasterSessionPhase, MasterSessionView, MasterTurnView,
};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind, SetupQuery, SetupView};
use crate::protocol::{ConfigId, CredentialRef, ProjectId, ProjectSpec, SessionId};
use crate::tui::client::settings::SettingsJob;
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::seam::{ProjectCard, Request, ShellRequest};
use crate::tui::shell::Shell;
use crate::tui::theme::{Appearance, Colors, Depth};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Position;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

fn ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = width;
    ctx.height = height;
    ctx
}

fn render(shell: &mut Shell, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn member(id: &str, kind: MemberKind) -> PoolMember {
    PoolMember {
        id: CredentialRef::new(id).unwrap(),
        kind,
        provider: kind.label().into(),
        status: MemberStatus::SignedIn,
        status_label: "Signed in".into(),
    }
}

async fn pump(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
            moved = true;
        }
        if !connection.busy()
            && shell.ctx.link == crate::tui::ctx::Link::Ready
            && let Some(request) = shell.ctx.next_request()
        {
            moved = true;
            match request {
                Request::Pools => {
                    sent.push("pools");
                    connection.shell(ShellRequest::Pools).unwrap();
                }
                Request::PoolProjects => {
                    sent.push("pool projects");
                    connection.shell(ShellRequest::PoolProjects).unwrap();
                }
                Request::Projects => {
                    sent.push("projects");
                    connection.shell(ShellRequest::Projects).unwrap();
                }
                Request::EditPool { change } => {
                    sent.push("edit pool");
                    let generation = shell.ctx.next_generation();
                    connection
                        .shell(ShellRequest::EditPool { change, generation })
                        .unwrap();
                }
                Request::Creation(job) => {
                    sent.push("create");
                    connection.creation(*job).unwrap();
                }
                Request::Intake(job) => {
                    sent.push("intake");
                    connection.intake(*job).unwrap();
                }
                Request::Master(job) => {
                    sent.push("master");
                    connection.master(*job).unwrap();
                }
                Request::Settings(job) => {
                    sent.push(match job.as_ref() {
                        SettingsJob::Harnesses => "harnesses",
                        _ => "settings",
                    });
                    connection.settings(*job).unwrap();
                }
                Request::Setup(query) => {
                    sent.push(match &query {
                        SetupQuery::Catalog { .. } => "catalog",
                        SetupQuery::Setup { .. } => "setup",
                        SetupQuery::Jobs { .. } => "jobs",
                        SetupQuery::Context { .. } => "context",
                        SetupQuery::Item { .. } => "item",
                        SetupQuery::Memories { .. } => "memories",
                        SetupQuery::Change { .. } => "change",
                        SetupQuery::ReviewMemory { .. } => "review",
                        SetupQuery::Team { .. } => "team",
                        SetupQuery::TeamChange { .. } => "team change",
                        SetupQuery::Library(_) => "library",
                        SetupQuery::Log { .. } => "log",
                        SetupQuery::ReadPaths { .. } => "read paths",
                        SetupQuery::AgentPolicy { .. } => "agent policy",
                        SetupQuery::CoordQuestions { .. } => "coord questions",
                    });
                    connection.project_setup(query).unwrap();
                }
                Request::Changes(query) => {
                    sent.push("changes");
                    connection.changes(*query).unwrap();
                }
                Request::TeamGraph { project } => {
                    sent.push("team graph");
                    connection.team_graph(project).unwrap();
                }
                Request::ProjectFiles { project, query } => {
                    sent.push("project files");
                    connection.project_files(project, query).unwrap();
                }
                Request::Process(query) => {
                    sent.push("process");
                    connection.process(*query).unwrap();
                }
                other => panic!("project pages never send {other:?}"),
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
  { id = "pool-b", name = "Spare", members = ["local-a"], projects = [], roles = [], fallback_allowed = false },
]
"#;

struct Engine {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    controller: OwnerController,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-page-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, SETTINGS).unwrap();
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
        let (_, mut controller) = OwnerController::attach_configured(client, OwnerClientKind::Tui)
            .await
            .unwrap();
        let home = crate::tui::test_home::projects_home(&mut controller, 1).await;
        Self {
            root,
            home,
            _service: service,
            _stop: stop,
            controller,
        }
    }

    async fn project(&mut self, id: &str, name: &str, generation: u64) -> ProjectId {
        let project = ProjectId::new(id).unwrap();
        let created = self
            .controller
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: name.into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: format!("{name} keeps notes."),
                    tech_stack: vec!["rust".into()],
                    platforms: vec![Platform::Cli],
                    research: vec![ResearchKind::CompetitorResearch],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                generation,
            )
            .await
            .unwrap();
        assert!(matches!(
            created,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ));
        project
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
}

#[test]
fn shell_keys_become_the_events_the_views_take() {
    let pairs = [
        (Key::Char('a'), KeyCode::Char('a'), KeyModifiers::NONE),
        (Key::Ctrl('j'), KeyCode::Char('j'), KeyModifiers::CONTROL),
        (Key::ShiftRight, KeyCode::Right, KeyModifiers::SHIFT),
        (Key::ShiftLeft, KeyCode::Left, KeyModifiers::SHIFT),
        (Key::BackTab, KeyCode::BackTab, KeyModifiers::SHIFT),
        (Key::PageDown, KeyCode::PageDown, KeyModifiers::NONE),
        (Key::Esc, KeyCode::Esc, KeyModifiers::NONE),
    ];
    for (key, code, modifiers) in pairs {
        let event = key_event(&key).unwrap();
        assert_eq!((event.code, event.modifiers), (code, modifiers), "{key:?}");
    }
    assert!(key_event(&Key::Paste("notes".into())).is_none());
}

#[test]
fn pool_choices_count_accounts_and_the_harnesses_they_run_on() {
    let pools = [PoolSummary {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        summary: "2 ChatGPT, 1 Local model".into(),
        members: vec![
            member("a-one", MemberKind::Chatgpt),
            member("a-two", MemberKind::Chatgpt),
            member("a-local", MemberKind::Local),
            member("a-claude", MemberKind::Claude),
        ],
        project_count: 0,
        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
    }];
    let choices = pool_choices_from(&pools);
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].name, "Main");
    assert_eq!(choices[0].accounts, 4);
    assert_eq!(
        choices[0].harnesses,
        [("codex".to_owned(), 2), ("claude".to_owned(), 1)]
    );
    assert_eq!(choices[0].accounts_for("codex"), 2);
}

#[test]
fn the_wizard_reads_harness_states_and_limits_from_the_real_catalog() {
    let bin = tempfile::Builder::new()
        .prefix("ca-tui-harnesses-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    for name in ["codex", "opencode", "cursor-agent"] {
        let path = bin.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entries = crate::harnesses::catalog(&crate::harnesses::Search::new(
        vec![bin.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let choices = harness_choices_from(&entries);
    assert_eq!(choices.len(), entries.len());
    let find = |id: &str| choices.iter().find(|choice| choice.id == id).unwrap();
    assert_eq!(
        find("codex"),
        &HarnessChoice {
            id: "codex".into(),
            name: "Codex".into(),
            state: HarnessState::Ready,
            limited: None,
        }
    );
    assert_eq!(find("claude-code").state, HarnessState::NotInstalled);
    assert_eq!(find("claude-code").limited, None);
    let opencode = find("opencode");
    assert_eq!(opencode.state, HarnessState::CantStart);
    assert_eq!(
        opencode.limited.as_deref(),
        Some("Limited control: installed, but CrazyAgent cannot start it yet.")
    );
    let cursor = find("cursor-agent");
    assert_eq!(cursor.state, HarnessState::CantStart);
    assert!(
        cursor
            .limited
            .as_deref()
            .is_some_and(|text| text.contains("Cursor's permission file")),
        "{cursor:?}"
    );
    let gemini = find("gemini-cli");
    assert_eq!(
        (gemini.state, gemini.limited.as_deref()),
        (HarnessState::NotInstalled, None)
    );
}

#[test]
fn a_project_folder_is_revealed_and_only_documents_inside_it_open() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-open-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let home = root.path().canonicalize().unwrap();
    let bundle = home.join("notes.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"), "<plist/>").unwrap();
    let crazy = bundle.join(".crazyagent");
    std::fs::create_dir_all(crazy.join("context/research/viewer.md")).unwrap();
    std::fs::create_dir_all(crazy.join("context/documents")).unwrap();
    std::fs::write(crazy.join("context/research/market.md"), "# Market").unwrap();
    std::fs::write(crazy.join("context/documents/brief.PDF"), "%PDF").unwrap();
    std::fs::write(
        crazy.join("context/research/report.command"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    std::fs::write(home.join("outside.md"), "kept outside").unwrap();
    std::os::unix::fs::symlink(
        home.join("outside.md"),
        crazy.join("context/documents/escape.md"),
    )
    .unwrap();
    let folder = |path: &std::path::Path| OpenRequest::Folder(path.display().to_string());
    let context = |path: &str| OpenRequest::Context {
        folder: crazy.display().to_string(),
        path: path.into(),
    };

    let refused = Err("CrazyAgent opens only documents inside the project folder.".to_owned());
    let let_through: Vec<(&str, Result<Opening, String>)> = [
        "context/research/report.command",
        "context/documents/escape.md",
        "context/research/viewer.md",
        "../outside.md",
        "context/../../../outside.md",
        "/etc/hosts",
        "context/research",
        "context/research/missing.md",
        "",
    ]
    .into_iter()
    .map(|path| (path, opening(&context(path))))
    .filter(|(_, got)| *got != refused)
    .collect();
    assert_eq!(let_through, Vec::new(), "every one is refused");
    assert_eq!(
        opening(&folder(&bundle)),
        Ok(Opening::Reveal(bundle.clone())),
        "a project folder is shown in the file manager, so a folder named notes.app is never launched"
    );
    assert_eq!(
        opening(&context("context/research/market.md")),
        Ok(Opening::Open(crazy.join("context/research/market.md")))
    );
    assert_eq!(
        opening(&context("context/documents/brief.PDF")),
        Ok(Opening::Open(crazy.join("context/documents/brief.PDF")))
    );
    let missing = Err("That file or folder is not there any more.".to_owned());
    assert_eq!(opening(&folder(std::path::Path::new("notes.app"))), missing);
    assert_eq!(opening(&folder(&home.join("gone"))), missing);
    assert_eq!(
        opening(&folder(&crazy.join("context/research/market.md"))),
        missing
    );
    assert_eq!(
        opening(&OpenRequest::Context {
            folder: "notes.app/.crazyagent".into(),
            path: "context/research/market.md".into(),
        }),
        missing
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_opens_a_real_project_and_applies_only_its_own_reads() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let other = engine.project("other-app", "Other App", 2).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(
        sent,
        [
            "pools",
            "pool projects",
            "setup",
            "jobs",
            "context",
            "memories"
        ]
    );
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("CrazyAgent  /  Home  /  Notes CLI"),
        "{shown}"
    );
    assert!(shown.contains("Notes CLI keeps notes."), "{shown}");
    assert!(
        shown.contains("This project has no pool, so no agent can start."),
        "{shown}"
    );

    connection
        .project_setup(SetupQuery::Setup {
            project: other.clone(),
        })
        .unwrap();
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Notes CLI keeps notes."), "{shown}");
    assert!(!shown.contains("Other App"), "{shown}");

    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Context,
        })
    );
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("brief.md"), "{shown}");
    assert!(shown.contains("Home  /  Notes CLI  /  Context"), "{shown}");

    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("This project has no pool, so the master cannot start."),
        "{shown}"
    );
    shell.key(Key::Char('h'));
    shell.key(Key::Enter);
    while let Some(request) = shell.ctx.next_request() {
        assert!(
            matches!(
                request,
                Request::Pools
                    | Request::PoolProjects
                    | Request::Master(_)
                    | Request::Changes(_)
                    | Request::TeamGraph { .. }
                    | Request::Setup(
                        SetupQuery::Jobs { .. }
                            | SetupQuery::Context { .. }
                            | SetupQuery::Memories { .. }
                    )
            ),
            "{request:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_settings_pick_a_pool_and_the_engine_assigns_it() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("No pool yet"), "{shown}");

    shell.key(Key::Enter);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Pick a pool"), "{shown}");
    assert!(shown.contains("( ) Main"), "{shown}");
    assert!(shown.contains("( ) Spare"), "{shown}");
    shell.key(Key::Down);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"edit pool"), "{sent:?}");
    assert!(sent.contains(&"pool projects"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Spare is assigned."), "{shown}");
    assert!(shown.contains("Spare   Enter Change"), "{shown}");

    shell.key(Key::Enter);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("(*) Spare"), "{shown}");
    shell.key(Key::Up);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Main is assigned."), "{shown}");
    assert!(shown.contains("Main   Enter Change"), "{shown}");

    shell.key(Key::Enter);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("(*) Main"), "{shown}");
    assert!(shown.contains("( ) No pool"), "{shown}");
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"edit pool"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("This project has no pool now."), "{shown}");
    assert!(shown.contains("No pool yet"), "{shown}");
}

#[test]
fn t_on_the_agents_tab_opens_the_project_team() {
    let notes = ProjectId::new("notes-cli").unwrap();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Agents,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    shell.key(Key::Char('t'));
    assert_eq!(
        shell.place(),
        Some(Place::Team {
            id: notes,
            name: "Notes CLI".into(),
        })
    );
    assert_eq!(shell.depth(), 2);
}

#[test]
fn an_at_file_read_dropped_on_a_full_queue_is_asked_again_on_the_next_key() {
    let notes = ProjectId::new("notes-cli").unwrap();
    let mut ctx = ctx(80, 24);
    ctx.link = crate::tui::ctx::Link::Ready;
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Conversation,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let files = |shell: &mut Shell| {
        let mut asked = Vec::new();
        while let Some(request) = shell.ctx.next_request() {
            if let Request::ProjectFiles { project, query } = request {
                assert_eq!(project, notes);
                asked.push(query);
            }
        }
        asked
    };
    files(&mut shell);
    let mut filled = 0;
    while filled < 64 && {
        let queued = shell.ctx.queued();
        shell.ctx.request(Request::TeamGraph {
            project: notes.clone(),
        });
        shell.ctx.queued() > queued
    } {
        filled += 1;
    }
    assert!(filled < 64, "the request queue is bounded");
    shell.key(Key::Char('@'));
    assert_eq!(
        files(&mut shell),
        Vec::<String>::new(),
        "the full queue dropped it"
    );
    shell.key(Key::Char('p'));
    assert_eq!(
        files(&mut shell),
        ["p"],
        "the dropped read is not counted as out"
    );
    shell.key(Key::Char('l'));
    assert_eq!(
        files(&mut shell),
        Vec::<String>::new(),
        "one read out at a time"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_is_edited_given_a_file_and_removed_through_the_engine() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let setup = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    let sources = tempfile::tempdir().unwrap();
    let scope = sources.path().join("scope.csv");
    std::fs::write(&scope, "item,size\nlogin,small\n").unwrap();
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.projects = Load::Ready(vec![ProjectCard {
        id: notes.clone(),
        name: "Notes CLI".into(),
        about: String::new(),
        platforms: vec![Platform::Cli],
        pool: None,
        research: Default::default(),
        created_ms: None,
        activity: Default::default(),
    }]);
    let home = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, home);
    while shell.ctx.next_request().is_some() {}
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Overview,
        })
    );
    pump(&mut shell, &mut connection).await;

    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('g'));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Edit project"), "{shown}");
    assert!(shown.contains("[ Notes CLI"), "{shown}");
    shell.key(Key::Paste(" Pro".into()));
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"change"), "{sent:?}");
    assert!(sent.contains(&"setup"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Changes saved."), "{shown}");
    assert!(shown.contains("Notes CLI Pro   g Edit details"), "{shown}");

    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('a'));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Add file"), "{shown}");
    shell.key(Key::Paste(scope.display().to_string()));
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"change"), "{sent:?}");
    assert!(sent.contains(&"context"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Document added."), "{shown}");
    assert!(shown.contains("scope.csv"), "{shown}");
    let copied = setup.state_folder().join("context/documents/scope.csv");
    assert_eq!(
        std::fs::read_to_string(&copied).unwrap(),
        "item,size\nlogin,small\n"
    );

    shell.key(Key::Char('a'));
    shell.key(Key::Paste(scope.display().to_string()));
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("The project already has a document with that name."),
        "{shown}"
    );
    shell.key(Key::Esc);

    shell.key(Key::ShiftRight);
    shell.key(Key::Char('x'));
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("Remove Notes CLI Pro from CrazyAgent?"),
        "{shown}"
    );
    shell.key(Key::Char('y'));
    let change = std::iter::from_fn(|| shell.ctx.next_request())
        .find_map(|request| match request {
            Request::Setup(query @ SetupQuery::Change { .. }) => Some(query),
            _ => None,
        })
        .unwrap();
    connection.project_setup(change).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while shell.place() != Some(Place::Home) {
        if let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
        }
        assert!(Instant::now() < deadline, "the removal never arrived");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(shell.depth(), 1);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Notes CLI Pro was removed."), "{shown}");
    assert!(copied.is_file(), "the folder stays on disk");

    while shell.ctx.next_request().is_some() {}
    let read = SetupQuery::Setup {
        project: notes.clone(),
    };
    connection.project_setup(read.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let after = loop {
        if let Some(OwnerEvent::Setup(event)) = connection.poll_owner().unwrap()
            && event.query == read
        {
            break event.result;
        }
        assert!(Instant::now() < deadline, "the setup read never arrived");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        !matches!(after, Ok(SetupView::Setup(Some(_)))),
        "the engine forgot the project: {after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_message_opens_the_master_and_reaches_the_engine() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Main is assigned."), "{shown}");

    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"master"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("Home  /  Notes CLI  /  Conversation"),
        "{shown}"
    );
    assert!(shown.contains("No messages yet."), "{shown}");

    shell.key(Key::Enter);
    for character in "Plan the first slice".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(
        sent.iter().filter(|each| **each == "master").count() >= 5,
        "{sent:?}"
    );
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Plan the first slice"), "{shown}");
    assert!(shown.contains("RCLI"), "{shown}");
    assert!(!shown.contains("The master could not start"), "{shown}");
    assert!(!shown.contains("did not take"), "{shown}");
    assert!(!shown.contains("No messages yet."), "{shown}");
    assert!(
        shown.contains("(rcli_profile_configuration_required)"),
        "{shown}"
    );
}

async fn latest_master(
    engine: &mut Engine,
    project: &ProjectId,
) -> (Option<MasterSessionView>, ConfigId) {
    let reply = engine
        .controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ProjectMaster { session } = reply.data else {
        panic!("a master read answers with the master session");
    };
    (session.map(|session| *session), reply.config_id)
}

async fn session_turns(
    engine: &mut Engine,
    project: &ProjectId,
    session: &SessionId,
) -> Vec<MasterTurnView> {
    let reply = engine
        .controller
        .read(OwnerRead::ProjectMasterTurns {
            project_id: project.clone(),
            session_id: session.clone(),
            cursor: None,
            limit: 32,
        })
        .await
        .unwrap();
    let OwnerData::ProjectMasterTurns { page } = reply.data else {
        panic!("a turn read answers with a turn page");
    };
    assert!(page.next.is_none());
    page.turns
}

async fn turn_text(engine: &mut Engine, project: &ProjectId, turn: &MasterTurnView) -> String {
    let reply = engine
        .controller
        .read(OwnerRead::ConversationHistory {
            project_id: project.clone(),
            cursor: None,
            limit: 32,
        })
        .await
        .unwrap();
    let OwnerData::ConversationHistory { page } = reply.data else {
        panic!("a conversation read answers with a history page");
    };
    page.entries
        .into_iter()
        .find(|entry| entry.event_cursor == turn.input_cursor)
        .expect("every master turn keeps its owner message")
        .text
}

async fn until(
    shell: &mut Shell,
    connection: &mut Connection,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        shell.refresh();
        pump(shell, connection).await;
        let shown = render(shell, 100, 40);
        if done(&shown) {
            return shown;
        }
        assert!(Instant::now() < deadline, "never shown:\n{shown}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn settled_master(
    engine: &mut Engine,
    project: &ProjectId,
    shell: &mut Shell,
    connection: &mut Connection,
    turns: usize,
) -> MasterSessionView {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        shell.refresh();
        pump(shell, connection).await;
        if let (Some(session), _) = latest_master(engine, project).await
            && session.current_turn.is_none()
            && session_turns(engine, project, &session.session_id)
                .await
                .len()
                == turns
        {
            return session;
        }
        assert!(Instant::now() < deadline, "the master turn never settled");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn say(shell: &mut Shell, connection: &mut Connection, text: &str) {
    shell.key(Key::Enter);
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    pump(shell, connection).await;
}

async fn conversation_with_a_settled_turn(
    engine: &mut Engine,
) -> (ProjectId, Shell, Connection, MasterSessionView) {
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(100, 40);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Main is assigned."), "{shown}");
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    say(&mut shell, &mut connection, "Plan the first slice").await;
    let first = settled_master(engine, &notes, &mut shell, &mut connection, 1).await;
    assert_eq!(first.phase, MasterSessionPhase::Configured);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Plan the first slice") && !shown.contains("Thinking")
    })
    .await;
    (notes, shell, connection, first)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_after_a_settings_change_lands_in_a_new_conversation() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;

    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    shell.key(Key::ShiftRight);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Spare is assigned.")
    })
    .await;
    let (open, changed) = latest_master(&mut engine, &notes).await;
    assert_eq!(open.as_ref(), Some(&first));
    assert_ne!(first.config_id, changed);

    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    say(&mut shell, &mut connection, "Add a dark theme").await;
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Settings changed, so a new conversation started.")
    })
    .await;
    assert!(!shown.contains("did not take"), "{shown}");
    let second = settled_master(&mut engine, &notes, &mut shell, &mut connection, 1).await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(second.phase, MasterSessionPhase::Configured);
    assert_eq!(second.config_id, latest_master(&mut engine, &notes).await.1);
    assert_eq!(second.selection.pool_ref.as_str(), "pool-b");
    let turns = session_turns(&mut engine, &notes, &second.session_id).await;
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turn_text(&mut engine, &notes, &turns[0]).await,
        "Add a dark theme"
    );
    let stale = session_turns(&mut engine, &notes, &first.session_id).await;
    assert_eq!(stale.len(), 1);
    assert_eq!(
        turn_text(&mut engine, &notes, &stale[0]).await,
        "Plan the first slice"
    );
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Add a dark theme") && !shown.contains("Thinking")
    })
    .await;
    assert!(!shown.contains("Plan the first slice"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e_ends_the_conversation_and_the_next_message_starts_a_new_one() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;

    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Stopped"), "{shown}");
    assert!(
        shown.contains("(rcli_profile_configuration_required)"),
        "{shown}"
    );
    assert!(!shown.contains("something went wrong"), "{shown}");
    assert!(!shown.contains("e End conversation"), "{shown}");
    shell.key(Key::Esc);
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("e End conversation"), "{shown}");
    shell.key(Key::Char('e'));
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("This session is parked.")
    })
    .await;
    assert!(!shown.contains("e End conversation"), "{shown}");
    let (closed, _) = latest_master(&mut engine, &notes).await;
    let closed = closed.unwrap();
    assert_eq!(closed.session_id, first.session_id);
    assert_eq!(closed.phase, MasterSessionPhase::Parked);
    assert_eq!(closed.current_turn, None);

    say(&mut shell, &mut connection, "Add a dark theme").await;
    let second = settled_master(&mut engine, &notes, &mut shell, &mut connection, 1).await;
    assert_ne!(second.session_id, first.session_id);
    assert_eq!(second.phase, MasterSessionPhase::Configured);
    let turns = session_turns(&mut engine, &notes, &second.session_id).await;
    assert_eq!(
        turn_text(&mut engine, &notes, &turns[0]).await,
        "Add a dark theme"
    );
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Add a dark theme") && !shown.contains("Thinking")
    })
    .await;
    assert!(!shown.contains("is closed"), "{shown}");
    assert!(!shown.contains("Settings changed"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resumed_session_shows_the_owner_s_earlier_messages() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    shell.key(Key::Esc);
    shell.key(Key::Char('e'));
    until(&mut shell, &mut connection, |shown| {
        shown.contains("This session is parked.")
    })
    .await;
    say(&mut shell, &mut connection, "Add a dark theme").await;
    let second = settled_master(&mut engine, &notes, &mut shell, &mut connection, 1).await;
    assert_ne!(second.session_id, first.session_id);
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Add a dark theme") && !shown.contains("Thinking")
    })
    .await;
    assert!(!shown.contains("Plan the first slice"), "{shown}");

    latest_master(&mut engine, &notes).await;
    engine
        .controller
        .begin_mutation(
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Resume {
                    project_id: notes.clone(),
                    session_id: first.session_id.clone(),
                    cancel_running: false,
                },
            },
            1,
        )
        .unwrap();
    let resumed = engine.controller.send_pending().await.unwrap();
    assert!(
        matches!(
            resumed,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ),
        "{resumed:?}"
    );
    let (live, _) = latest_master(&mut engine, &notes).await;
    assert_eq!(live.unwrap().session_id, first.session_id);
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Plan the first slice")
    })
    .await;
    assert!(!shown.contains("Add a dark theme"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_starts_a_new_session_and_the_sessions_list_resumes_the_old_one() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    shell.key(Key::Esc);
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    assert!(shown.contains("n New session"), "{shown}");
    assert!(!shown.contains("earlier"), "{shown}");

    shell.key(Key::Char('n'));
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("1 earlier") && shown.contains("No messages yet.")
    })
    .await;
    assert!(!shown.contains("RCLI stopped"), "{shown}");
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    let fresh = fresh.unwrap();
    assert_ne!(fresh.session_id, first.session_id);
    assert_eq!(fresh.phase, MasterSessionPhase::Configured);

    shell.key(Key::Char('l'));
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Sessions  2"), "{shown}");
    assert!(shown.contains("Enter Resume"), "{shown}");
    let parked = shown
        .lines()
        .find(|line| line.contains("Parked"))
        .unwrap_or_else(|| panic!("no parked row:\n{shown}"));
    assert!(parked.contains("Plan the first slice"), "{shown}");
    if !parked.contains("> Parked") {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("RCLI stopped") && !shown.contains("Sessions  2")
    })
    .await;
    assert!(shown.contains("1 earlier"), "{shown}");
    assert!(!shown.contains("No messages yet."), "{shown}");
    assert!(shown.contains("Plan the first slice"), "{shown}");
    let (resumed, _) = latest_master(&mut engine, &notes).await;
    let resumed = resumed.unwrap();
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
}

/// A new session on a project whose master runs live turns the live switch on once the
/// engine has made the new session, so the live master starts on the fresh conversation
/// even when it was stopped or never started.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_session_on_a_live_master_turns_the_switch_on() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    let before = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_ne!(before.live_master, Some(true), "the switch starts off");
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;

    shell.key(Key::Char('n'));
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !sent.contains(&"team change") {
        assert!(
            Instant::now() < deadline,
            "the switch never turned on: {sent:?}"
        );
        shell.refresh();
        sent.extend(pump(&mut shell, &mut connection).await);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let new = sent.iter().position(|label| *label == "master");
    let change = sent.iter().position(|label| *label == "team change");
    assert!(
        new.is_some() && new < change,
        "the switch turns on after the new session is asked for: {sent:?}"
    );
    until(&mut shell, &mut connection, |shown| {
        shown.contains("No messages yet.")
    })
    .await;
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    assert_ne!(fresh.unwrap().session_id, first.session_id);
    let after = engine.controller.team_status(notes.clone()).await.unwrap();
    assert!(
        after.live_master.is_some(),
        "the engine took the switch change for a live-capable master: {:?} {:?}",
        after.live_master,
        after.live_problem
    );
}

/// A new session asked for while no session is in view opens the master, and once the
/// engine confirms that open the live switch turns on, as it does for a new session of a
/// master that already had one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_session_with_no_session_in_view_turns_the_live_switch_on() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(100, 40);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Main is assigned."), "{shown}");
    for _ in 0..4 {
        shell.key(Key::ShiftLeft);
    }
    pump(&mut shell, &mut connection).await;
    let (none, _) = latest_master(&mut engine, &notes).await;
    assert!(none.is_none(), "the project has no master session yet");
    let before = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_ne!(before.live_master, Some(true), "the switch starts off");

    shell.key(Key::Char('n'));
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !sent.contains(&"team change") {
        assert!(
            Instant::now() < deadline,
            "the switch never turned on: {sent:?}"
        );
        shell.refresh();
        sent.extend(pump(&mut shell, &mut connection).await);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let open = sent.iter().position(|label| *label == "master");
    let change = sent.iter().position(|label| *label == "team change");
    assert!(
        open.is_some() && open < change,
        "the switch turns on after the master opens: {sent:?}"
    );
    let (opened, _) = latest_master(&mut engine, &notes).await;
    assert!(opened.is_some(), "the new session opened the master");
    let after = engine.controller.team_status(notes.clone()).await.unwrap();
    assert!(
        after.live_master.is_some(),
        "the engine took the switch change: {:?} {:?}",
        after.live_master,
        after.live_problem
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_project_with_an_open_master_conversation_ends_it_and_lands_on_home() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.projects = Load::Ready(vec![ProjectCard {
        id: notes.clone(),
        name: "Notes CLI".into(),
        about: String::new(),
        platforms: vec![Platform::Cli],
        pool: None,
        research: Default::default(),
        created_ms: None,
        activity: Default::default(),
    }]);
    let home = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, home);
    while shell.ctx.next_request().is_some() {}
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    for _ in 0..4 {
        shell.key(Key::ShiftLeft);
    }
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    for character in "Plan the first slice".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Esc);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Plan the first slice"), "{shown}");
    assert!(shown.contains("e End conversation"), "{shown}");
    let (open, _) = latest_master(&mut engine, &notes).await;
    let open = open.expect("the first message opened a master conversation");
    assert_eq!(open.phase, MasterSessionPhase::Configured);

    for _ in 0..4 {
        shell.key(Key::ShiftRight);
    }
    shell.key(Key::Char('x'));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Its master conversation ends"), "{shown}");
    shell.key(Key::Char('y'));
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert_eq!(shell.place(), Some(Place::Home), "{shown}");
    assert!(shown.contains("Notes CLI was removed."), "{shown}");

    let (closed, _) = latest_master(&mut engine, &notes).await;
    let closed = closed.expect("the ended conversation stays on record");
    assert_eq!(closed.session_id, open.session_id);
    assert_eq!(closed.phase, MasterSessionPhase::Closed);
    assert!(closed.current_turn.is_none());

    while shell.ctx.next_request().is_some() {}
    let read = SetupQuery::Setup {
        project: notes.clone(),
    };
    connection.project_setup(read.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let after = loop {
        if let Some(OwnerEvent::Setup(event)) = connection.poll_owner().unwrap()
            && event.query == read
        {
            break event.result;
        }
        assert!(Instant::now() < deadline, "the setup read never arrived");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        !matches!(after, Ok(SetupView::Setup(Some(_)))),
        "the engine forgot the project: {after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wizard_waits_for_an_engine_slow_to_accept_its_connection() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let engine = runtime.spawn(Engine::start()).await.unwrap();
    let (blocking, blocked) = std::sync::mpsc::channel();
    let busy = runtime.spawn(async move {
        blocking.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
    });
    blocked.recv().unwrap();
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, &mut connection).await;
    busy.await.unwrap();
    drop(connection);
    std::thread::spawn(move || {
        drop(engine);
        runtime.shutdown_timeout(Duration::from_secs(10));
    })
    .join()
    .unwrap();
    assert_eq!(sent, ["pools", "harnesses", "catalog", "catalog"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wizard_creates_a_real_project_assigns_its_pool_and_opens_it() {
    let engine = Engine::start().await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["pools", "harnesses", "catalog", "catalog"]);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("New project  /  About"), "{shown}");
    assert!(
        !shown.contains("This part of CrazyAgent is still being built"),
        "{shown}"
    );

    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    assert!(shell.ctx.next_request().is_none());
    shell.key(Key::ShiftRight);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("New project  /  Stack and platforms"),
        "{shown}"
    );
    assert!(shown.contains("Rust"), "{shown}");

    shell.key(Key::ShiftLeft);
    shell.key(Key::ShiftLeft);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool"), "{shown}");
    assert!(shown.contains("Main"), "{shown}");
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(&sent[..2], ["create", "projects"], "{sent:?}");
    assert!(sent.contains(&"edit pool"), "{sent:?}");
    let Some(Place::Project { id, name, tab }) = shell.place() else {
        panic!("the new project opens, got {:?}", shell.place());
    };
    assert_eq!((name.as_str(), tab), ("Tidepool", ProjectTab::Overview));
    assert!(id.as_str().starts_with("project-"));
    assert_eq!(kept_draft(), None);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool is ready."), "{shown}");
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Home  /  Tidepool"), "{shown}");
    assert!(shown.contains("Main  1 account"), "{shown}");
    assert!(!shown.contains("no pool"), "{shown}");
    let folder = format!("{}/tidepool", engine.home.path().display());
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with("  Tidepool  ") && line.trim_end().ends_with(&folder)),
        "the title row shows the name and the folder in the projects home: {shown}"
    );
}

fn go_to(shell: &mut Shell, query: &str) {
    shell.key(Key::Ctrl('k'));
    for character in query.chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_run_in_the_background_finishes_after_its_page_is_gone() {
    let engine = Engine::start().await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::ShiftLeft);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("New project  /  Review"), "{shown}");
    assert!(shown.contains("Main"), "{shown}");
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home), "Run in background");
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1, "Home drops the buried wizard");
    assert_eq!(shell.place(), Some(Place::Home));

    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"create"), "{sent:?}");
    assert!(
        sent.contains(&"edit pool"),
        "the picked pool is assigned without the page: {sent:?}"
    );
    assert!(sent.contains(&"pool projects"), "{sent:?}");
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready.")
    );
    let tidepool = shell
        .ctx
        .store
        .projects
        .ready()
        .and_then(|cards| cards.iter().find(|card| card.name == "Tidepool"))
        .map(|card| card.id.clone())
        .expect("the project is listed");
    let scopes = shell.ctx.store.pool_projects.ready().cloned().unwrap();
    let main = scopes
        .iter()
        .find(|scope| scope.pool.as_str() == "pool-a")
        .expect("pool Main has a scope");
    assert!(
        main.projects
            .iter()
            .any(|id| id.as_str() == tidepool.as_str()),
        "the engine assigned pool Main to the project: {scopes:?}"
    );
    assert_eq!(kept_draft(), None, "a made project keeps no draft");

    go_to(&mut shell, "new project");
    assert_eq!(shell.place(), Some(Place::NewProject));
    pump(&mut shell, &mut connection).await;
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"create"), "{sent:?}");
    assert!(!sent.contains(&"edit pool"), "{sent:?}");
    let refused = shell.ctx.message().expect("the refusal is shown");
    assert!(
        refused.lead.starts_with("Could not create"),
        "{}",
        refused.lead
    );
    assert_eq!(
        kept_draft().as_deref(),
        Some("Tidepool"),
        "the refused draft is kept for the next New project"
    );
}

#[test]
fn leaving_the_wizard_keeps_the_draft_for_next_time() {
    let mut ctx = ctx(80, 24);
    let mut screen = open(&Place::NewProject, &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(matches!(screen.key(&Key::Esc, &mut ctx), Flow::Back));
    drop(screen);
    assert_eq!(kept_draft(), None);

    let mut screen = open(&Place::NewProject, &mut ctx);
    for character in "Fieldnotes".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    assert!(screen.draft());
    screen.key(&Key::Esc, &mut ctx);
    assert!(matches!(screen.key(&Key::Esc, &mut ctx), Flow::Back));
    drop(screen);
    assert_eq!(kept_draft().as_deref(), Some("Fieldnotes"));

    let mut screen = open(&Place::NewProject, &mut ctx);
    assert_eq!(kept_draft(), None);
    assert!(screen.draft());
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            screen.draw(frame, frame.area(), &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let shown: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(shown.contains("Fieldnotes"), "{shown}");
    drop(screen);
    assert_eq!(kept_draft().as_deref(), Some("Fieldnotes"));
}

#[test]
fn an_unsent_message_counts_as_a_draft_that_quit_warns_about() {
    let mut ctx = ctx(80, 24);
    let mut screen = open(
        &Place::Project {
            id: ProjectId::new("notes-cli").unwrap(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Conversation,
        },
        &mut ctx,
    );
    assert!(!screen.draft());
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.typing());
    for character in "   ".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    assert!(!screen.draft());
    for character in "Plan it".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    assert!(screen.draft());
    assert_eq!(screen.loss(), "Your unsent message will be lost.");
    assert!(matches!(screen.key(&Key::Enter, &mut ctx), Flow::Handled));
    assert!(screen.draft());
    while let Some(request) = ctx.next_request() {
        assert!(
            !matches!(request, Request::Master(_)),
            "nothing is sent without a pool"
        );
    }
}

fn framed(shell: &mut Shell, width: u16, height: u16) -> String {
    let away = Position::new(u16::MAX, u16::MAX);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.backend_mut().set_cursor_position(away).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let cursor = terminal
        .backend_mut()
        .get_cursor_position()
        .ok()
        .filter(|position| *position != away);
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if cursor == Some(Position::new(x, y)) {
                line.push('\u{2588}');
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            skip = cells(symbol).saturating_sub(1);
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

#[test]
fn shell_draws_the_wizard_like_the_design() {
    let about = "A live dashboard for marine researchers to watch sensor buoys and get alerts when readings look wrong.";
    let shell_at = |width: u16, height: u16| {
        let mut ctx = ctx(width, height);
        ctx.store.pools = Load::Ready(Vec::new());
        let screen = open(&Place::NewProject, &mut ctx);
        Shell::with(ctx, screen)
    };
    assert_eq!(kept_draft(), None);
    let mut shell = shell_at(120, 36);
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    for character in about.chars() {
        shell.key(Key::Char(character));
    }
    let mut drawn = vec![("wiz-about-120".to_owned(), framed(&mut shell, 120, 36))];
    drop(shell);
    assert_eq!(kept_draft().as_deref(), Some("Tidepool"));

    let mut shell = shell_at(80, 24);
    assert_eq!(kept_draft(), None);
    drawn.push(("wiz-about-80".to_owned(), framed(&mut shell, 80, 24)));
    check_frames(&drawn);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_the_engine_cannot_write_into_the_project_says_why() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let setup = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    let documents = setup.state_folder().join("context/documents");
    std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o500)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    let scope = sources.path().join("scope.csv");
    std::fs::write(&scope, "item,size\nlogin,small\n").unwrap();
    let mut connection = engine.connect();
    let mut ctx = ctx(100, 24);
    ctx.store.projects = Load::Ready(vec![ProjectCard {
        id: notes.clone(),
        name: "Notes CLI".into(),
        about: String::new(),
        platforms: vec![Platform::Cli],
        pool: None,
        research: Default::default(),
        created_ms: None,
        activity: Default::default(),
    }]);
    let home = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, home);
    while shell.ctx.next_request().is_some() {}
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('a'));
    let shown = render(&mut shell, 100, 24);
    assert!(shown.contains("Add file"), "{shown}");
    shell.key(Key::Paste(scope.display().to_string()));
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(sent.first(), Some(&"change"), "{sent:?}");
    let shown = render(&mut shell, 100, 24);
    assert!(
        shown.contains("CrazyAgent can't write in the project folder."),
        "{shown}"
    );
    assert!(shown.contains("permissions."), "{shown}");
    assert!(!shown.contains("That didn't work"), "{shown}");
    assert!(!documents.join("scope.csv").exists(), "nothing was copied");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_folder_the_engine_cannot_make_says_why_in_the_wizard() {
    let engine = Engine::start().await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::ShiftLeft);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool"), "{shown}");
    std::fs::set_permissions(engine.home.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    std::fs::set_permissions(engine.home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sent.first(), Some(&"create"), "{sent:?}");
    assert_eq!(shell.place(), Some(Place::NewProject));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Could not create the project."), "{shown}");
    assert!(
        shown.contains("The project folder could not be created. Nothing was kept."),
        "{shown}"
    );
    assert!(!engine.home.path().join("tidepool").exists(), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_at_word_in_the_console_lists_the_project_files_through_the_engine() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let setup = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    let folder = std::path::PathBuf::from(&setup.folder);
    let status = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "-q", "--template="])
            .current_dir(&folder)
            .env_clear()
            .env("HOME", engine.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1"),
    )
    .unwrap()
    .status;
    assert!(status.success());
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("src/planner.rs"), "fn plan() {}\n").unwrap();
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Main is assigned."), "{shown}");
    for _ in 0..4 {
        shell.key(Key::ShiftLeft);
    }
    let sent = pump(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"project files"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("Home  /  Notes CLI  /  Conversation")
            && !shown.contains("This project has no pool"),
        "{shown}"
    );
    for typed in "ask @pl".chars() {
        shell.key(Key::Char(typed));
    }
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(
        sent.iter().filter(|name| **name == "project files").count(),
        2,
        "one read for the bare @, then one for the word typed while it was out: {sent:?}"
    );
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("@src/planner.rs"), "{shown}");
    shell.key(Key::Tab);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("> ask @src/planner.rs"), "{shown}");
    for typed in "now".chars() {
        shell.key(Key::Char(typed));
    }
    let sent = pump(&mut shell, &mut connection).await;
    assert!(
        !sent.contains(&"project files"),
        "no read while no @ word is typed: {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_changes_tab_reads_the_project_change_sets_from_the_engine() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Changes,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"changes"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Home  /  Notes CLI  /  Changes"), "{shown}");
    assert!(shown.contains("No changes yet."), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_t_on_the_conversation_input_opens_team_and_keeps_the_draft() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut first = ctx(120, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut first,
    );
    let mut settings = Shell::with(first, screen);
    pump(&mut settings, &mut connection).await;
    settings.key(Key::Enter);
    settings.key(Key::Enter);
    pump(&mut settings, &mut connection).await;
    let shown = render(&mut settings, 120, 24);
    assert!(shown.contains("Main is assigned."), "{shown}");
    drop(settings);
    drop(connection);

    let mut connection = engine.connect();
    let mut ctx = ctx(120, 24);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Conversation,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 24);
    assert!(shown.contains("No messages yet."), "{shown}");
    let conversation = shell.place();
    for character in "start the sync".chars() {
        shell.key(Key::Char(character));
    }
    assert_eq!(
        shell.place(),
        conversation,
        "t types while the input is focused"
    );
    let shown = render(&mut shell, 120, 24);
    assert!(cell_reads(&shown, "> start the sync"), "{shown}");
    assert!(shown.contains("Ctrl-T Team"), "{shown}");

    shell.key(Key::Ctrl('t'));
    assert_eq!(
        shell.place(),
        Some(Place::Team {
            id: notes,
            name: "Notes CLI".into(),
        })
    );
    assert_eq!(shell.depth(), 2);

    shell.key(Key::Esc);
    assert_eq!(shell.place(), conversation);
    let shown = render(&mut shell, 120, 24);
    assert!(cell_reads(&shown, "> start the sync"), "{shown}");
}

fn cell_reads(shown: &str, text: &str) -> bool {
    shown
        .lines()
        .any(|line| line.split('│').any(|cell| cell.trim() == text))
}

fn team_shell(project: &ProjectId) -> Shell {
    let mut ctx = ctx(200, 40);
    let screen = open(
        &Place::Team {
            id: project.clone(),
            name: "Notes CLI".into(),
        },
        &mut ctx,
    );
    Shell::with(ctx, screen)
}

async fn team_settings(shell: &mut Shell, connection: &mut Connection) {
    shell.key(Key::Char('s'));
    let sent = pump(shell, connection).await;
    assert_eq!(sent, ["team"]);
    let shown = render(shell, 200, 40);
    assert!(shown.contains("Team settings"), "{shown}");
}

fn seed_question(engine: &Engine, project: &ProjectId, text: &str) {
    let connection = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
             VALUES (?1, 'sub', 'codex', 0, 'stopped', 1, 1)",
            [project.as_str()],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    crate::coord::send(
        &connection,
        crate::coord::NewMessage {
            project: project.clone(),
            from: crate::coord::Party::Agent(agent),
            to: crate::coord::Party::Owner,
            kind: crate::coord::MessageKind::Question,
            question: Some((
                crate::coord::QuestionFor::Me,
                crate::coord::Severity::Normal,
            )),
            work: None,
            reply_to: None,
            body: text.into(),
            idempotency_key: None,
        },
        2,
    )
    .unwrap();
}

#[tokio::test]
async fn the_team_screen_opens_its_settings_on_s_and_esc_returns_to_the_tree() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut shell = team_shell(&notes);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"team graph"), "{sent:?}");
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("s Settings"), "{shown}");
    assert!(!shown.contains("Team settings"), "{shown}");

    team_settings(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Keep warm  Off"), "{shown}");
    assert!(shown.contains("Spread accounts  Off"), "{shown}");

    shell.key(Key::Esc);
    let shown = render(&mut shell, 200, 40);
    assert!(!shown.contains("Team settings"), "{shown}");
    assert!(shown.contains("s Settings"), "{shown}");
    assert_eq!(
        shell.place(),
        Some(Place::Team {
            id: notes,
            name: "Notes CLI".into(),
        })
    );
}

#[tokio::test]
async fn a_settings_key_on_the_team_screen_sends_a_team_change() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut shell = team_shell(&notes);
    pump(&mut shell, &mut connection).await;
    team_settings(&mut shell, &mut connection).await;
    assert!(
        !engine
            .controller
            .team_status(notes.clone())
            .await
            .unwrap()
            .keep_warm
    );

    shell.key(Key::Char('w'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["team change", "team"]);
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Keep warm  On"), "{shown}");
    assert!(
        engine
            .controller
            .team_status(notes)
            .await
            .unwrap()
            .keep_warm
    );
}

#[tokio::test]
async fn a_paste_on_the_team_settings_goes_into_the_task_draft_and_quit_warns_it_is_lost() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut shell = team_shell(&notes);
    pump(&mut shell, &mut connection).await;
    team_settings(&mut shell, &mut connection).await;

    shell.key(Key::Char('n'));
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("New task from you"), "{shown}");
    shell.key(Key::Paste("Fix the flaky sync test".into()));
    shell.key(Key::Char('s'));
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Fix the flaky sync tests"), "{shown}");
    assert!(pump(&mut shell, &mut connection).await.is_empty());

    shell.key(Key::Ctrl('q'));
    assert!(!shell.quitting());
    let shown = render(&mut shell, 200, 40);
    assert!(
        shown.contains("The task you are writing will be lost."),
        "{shown}"
    );
}

#[tokio::test]
async fn the_team_settings_reach_the_coordination_editor_and_spread_accounts() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut shell = team_shell(&notes);
    pump(&mut shell, &mut connection).await;
    team_settings(&mut shell, &mut connection).await;

    shell.key(Key::Char('c'));
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Workflow nudges"), "{shown}");
    assert!(shown.contains("Counted tools"), "{shown}");
    shell.key(Key::Esc);
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Team settings"), "{shown}");
    assert!(!shown.contains("Counted tools"), "{shown}");
    shell.key(Key::Esc);
    let shown = render(&mut shell, 200, 40);
    assert!(!shown.contains("Team settings"), "{shown}");
    assert!(shown.contains("s Settings"), "{shown}");

    team_settings(&mut shell, &mut connection).await;
    assert!(
        !engine
            .controller
            .team_status(notes.clone())
            .await
            .unwrap()
            .spread_accounts
    );
    shell.key(Key::Char('P'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["team change", "team"]);
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Spread accounts  On"), "{shown}");
    assert!(
        engine
            .controller
            .team_status(notes)
            .await
            .unwrap()
            .spread_accounts
    );
}

#[tokio::test]
async fn the_team_settings_and_questions_do_not_take_each_others_keys() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    seed_question(&engine, &notes, "Which port does the buoy feed use?");
    let mut connection = engine.connect();
    let mut shell = team_shell(&notes);
    pump(&mut shell, &mut connection).await;
    team_settings(&mut shell, &mut connection).await;

    shell.key(Key::Char('q'));
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Team settings"), "{shown}");
    assert!(!shown.contains("Which port"), "{shown}");

    shell.key(Key::Esc);
    shell.key(Key::Char('q'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["coord questions"]);
    let shown = render(&mut shell, 200, 40);
    assert!(
        shown.contains("Which port does the buoy feed use?"),
        "{shown}"
    );
    shell.key(Key::Char('s'));
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    let shown = render(&mut shell, 200, 40);
    assert!(!shown.contains("Team settings"), "{shown}");

    shell.key(Key::Enter);
    for character in "use 4000 so?[]".chars() {
        shell.key(Key::Char(character));
    }
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    let shown = render(&mut shell, 200, 40);
    assert!(cell_reads(&shown, "use 4000 so?[]"), "{shown}");
    assert!(!shown.contains("Team settings"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_memory_imports_a_real_folder_into_the_chosen_project() {
    let mut engine = Engine::start().await;
    let parent = tempfile::Builder::new()
        .prefix("ca-tui-import-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let folder = parent.path().join("memory");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a.md"), "Plan meals on Sunday.").unwrap();
    std::fs::write(
        folder.join("b.md"),
        "---\ntitle: shop\n---\nShop on Monday.",
    )
    .unwrap();
    std::fs::write(folder.join("long.md"), "l".repeat(5000)).unwrap();
    std::fs::write(folder.join("MEMORY.md"), "- [a](a.md)").unwrap();

    {
        let mut connection = engine.connect();
        let mut ctx = ctx(120, 40);
        let screen = open(&Place::Settings(SettingsSection::Memory), &mut ctx);
        let mut shell = Shell::with(ctx, screen);
        pump(&mut shell, &mut connection).await;
        shell.key(Key::Char('i'));
        let shown = render(&mut shell, 120, 40);
        assert!(shown.contains("Add a project first."), "{shown}");
        assert!(!shown.contains("Import memory"), "{shown}");
    }

    let notes = engine.project("notes-cli", "Notes CLI", 2).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(120, 40);
    ctx.home = Some(parent.path().to_str().unwrap().into());
    let screen = open(&Place::Settings(SettingsSection::Memory), &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("i Import"), "{shown}");

    shell.key(Key::Char('i'));
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("Import memory"), "{shown}");
    assert!(shown.contains("Notes CLI"), "{shown}");
    for character in "memory".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("Use the full path of the folder"), "{shown}");

    for _ in "memory".chars() {
        shell.key(Key::Backspace);
    }
    for character in "~/memory".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"settings"), "{sent:?}");
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("Imported 2, skipped 1."), "{shown}");
    assert!(!shown.contains("Import memory"), "{shown}");
    assert!(shown.contains("Shop on Monday."), "{shown}");
    assert!(shown.contains("Plan meals on Sunday."), "{shown}");
    assert!(
        shown.contains("Imported from b.md for Notes CLI only.")
            || shown.contains("Imported from a.md for Notes CLI only."),
        "{shown}"
    );

    let page = engine
        .controller
        .memories(
            crate::memory::library::MemoryScope::Project {
                project_id: notes.clone(),
            },
            None,
            None,
        )
        .await
        .unwrap();
    let mut files: Vec<String> = page
        .items
        .iter()
        .filter_map(|memory| match &memory.origin {
            crate::memory::library::MemoryOrigin::Imported { file } => Some(file.clone()),
            _ => None,
        })
        .collect();
    files.sort();
    assert_eq!(files, ["a.md", "b.md"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tools_page_switches_real_tools_and_ignores_reads_for_another_scope() {
    use super::settings_plugins::{ToolsPage, plugin_plain};
    use crate::coord::tool_switches::Switch;
    use crate::plugins::{Scope, ServerSpec};
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    for (project, tool, switch) in [
        (None, "start_work", Switch::Off),
        (None, "recall", Switch::Hidden),
        (Some(notes.clone()), "start_work", Switch::On),
    ] {
        engine
            .controller
            .set_tool_switch(project, None, tool.into(), switch)
            .await
            .unwrap();
    }
    let folder = engine.root.path().join("servers");
    std::fs::create_dir(&folder).unwrap();
    let command = folder.join("mcp_fixture.py");
    std::fs::write(
        &command,
        include_str!("../../../tests/fixtures/mcp_fixture.py"),
    )
    .unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = engine
        .controller
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            ServerSpec::Stdio {
                command: command.to_str().unwrap().into(),
                args: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let checked = engine
        .controller
        .check_mcp_server(server.id.clone())
        .await
        .unwrap();
    assert_eq!(checked.tools, ["echo", "anonymous"]);

    let mut connection = engine.connect();
    let page = ToolsPage::new(vec![(server.id.clone(), "fixture".into())], 0);
    let mut shell = Shell::with(ctx(120, 30), Box::new(page));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"settings"), "{sent:?}");
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("CrazyAgent tools  1 of 2"), "{shown}");
    assert!(shown.contains("start_work"), "{shown}");
    assert!(!shown.contains("recall"), "{shown}");
    assert!(
        shown.contains("1 deleted. Press r to restore the defaults and bring it back."),
        "{shown}"
    );
    assert!(!shown.contains("On here"), "{shown}");

    connection
        .settings(SettingsJob::Tools {
            project: Some(notes.clone()),
            server: None,
        })
        .unwrap();
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 30);
    assert!(
        !shown.contains("On here"),
        "a read for one project never lands on the every-project page: {shown}"
    );

    shell.key(Key::Char(' '));
    shell.key(Key::Tab);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["settings"]);
    let shown = render(&mut shell, 120, 30);
    assert!(
        shown.contains("start_work is on for every project."),
        "{shown}"
    );
    assert!(
        shown.contains("CrazyAgent tools  1 of 2"),
        "Tab while a change runs keeps the server: {shown}"
    );
    let stored = engine.controller.tool_switches(None, None).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .find(|view| view.tool == "start_work")
            .unwrap()
            .global,
        Switch::On
    );

    shell.key(Key::Tab);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("fixture  2 of 2"), "{shown}");
    assert!(
        shown.contains("echo") && shown.contains("anonymous"),
        "{shown}"
    );
    shell.key(Key::Char('d'));
    assert!(pump(&mut shell, &mut connection).await.is_empty());
    let shown = render(&mut shell, 120, 30);
    assert!(
        shown.contains("Only a CrazyAgent tool can be deleted."),
        "{shown}"
    );

    engine
        .controller
        .remove_mcp_server(server.id.clone())
        .await
        .unwrap();
    shell.key(Key::Char(' '));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["settings", "settings"], "the change, then a reload");
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains(plugin_plain("server_not_found")), "{shown}");
    assert!(shown.contains("Could not refresh the tools."), "{shown}");
}

async fn owner_ready(connection: &mut Connection) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut ready = false;
    loop {
        while let Some(event) = connection.poll_owner().unwrap() {
            if let OwnerEvent::Access { ready: true, .. } = event {
                ready = true;
            }
        }
        if ready && !connection.busy() {
            return;
        }
        assert!(Instant::now() < deadline, "the owner link never got ready");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn settings_reply(
    connection: &mut Connection,
    job: SettingsJob,
) -> crate::tui::client::settings::SettingsEvent {
    connection.settings(job).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        while let Some(event) = connection.poll_owner().unwrap() {
            if let OwnerEvent::Settings(event) = event {
                return *event;
            }
        }
        assert!(Instant::now() < deadline, "no settings reply");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_jobs_round_trip_through_the_tui_client_with_their_changed_flags() {
    use crate::coord::tool_switches::Switch;
    use crate::tui::client::settings::SettingsEvent;
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    owner_ready(&mut connection).await;
    let jobs = [
        (
            SettingsJob::Tools {
                project: Some(notes.clone()),
                server: None,
            },
            false,
            None,
        ),
        (
            SettingsJob::SetTool {
                project: Some(notes.clone()),
                server: None,
                tool: "start_work".into(),
                switch: Switch::Off,
            },
            true,
            Some(Switch::Off),
        ),
        (
            SettingsJob::RestoreTools {
                project: Some(notes.clone()),
                server: None,
            },
            true,
            None,
        ),
    ];
    for (job, flag, here) in jobs {
        let SettingsEvent::Tools {
            project,
            server,
            changed,
            result,
        } = settings_reply(&mut connection, job).await
        else {
            panic!("a tools reply expected");
        };
        assert_eq!(
            (project, server, changed),
            (Some(notes.clone()), None, flag)
        );
        let tools = result.unwrap();
        let start = tools.iter().find(|view| view.tool == "start_work").unwrap();
        assert_eq!(start.here, here);
        owner_ready_after(&mut connection).await;
    }
    let SettingsEvent::Tools {
        changed, result, ..
    } = settings_reply(
        &mut connection,
        SettingsJob::SetTool {
            project: None,
            server: None,
            tool: "nope".into(),
            switch: Switch::Off,
        },
    )
    .await
    else {
        panic!("a tools reply expected");
    };
    assert!(changed);
    assert_eq!(result.unwrap_err(), "tool_unknown");
}

async fn owner_ready_after(connection: &mut Connection) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while connection.busy() {
        while connection.poll_owner().unwrap().is_some() {}
        assert!(Instant::now() < deadline, "the owner job never finished");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
