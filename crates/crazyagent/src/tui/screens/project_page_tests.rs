use super::new_project::{HarnessChoice, HarnessState};
use super::new_project_frames::check_frames;
use super::new_project_input::cells;
use super::new_project_page::{harness_choices_from, kept_draft, key_event, pool_choices_from};
use super::project::OpenRequest;
use super::project_page::{BANNER_ROWS_MAX, Opening, changed_file, move_banner, opening};
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
use crate::project_setup::{Platform, ProjectSetup, ResearchKind, SetupQuery, SetupView, TeamPlan};
use crate::protocol::{ConfigId, CredentialRef, ProjectId, ProjectSpec, SessionId};
use crate::tui::client::process::ProcessQuery;
use crate::tui::client::questions::QuestionsQuery;
use crate::tui::client::settings::SettingsJob;
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Message, Tone};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::master_session::{Event as MasterEvent, Job as MasterJob};
use crate::tui::run::owner_arrival;
use crate::tui::seam::{Arrival, ProjectCard, Request, ShellRequest};
use crate::tui::shell::Shell;
use crate::tui::terminal_pane::LEADER;
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
                        SetupQuery::Removal { .. } => "removal",
                        SetupQuery::Context { .. } => "context",
                        SetupQuery::Item { .. } => "item",
                        SetupQuery::Memories { .. } => "memories",
                        SetupQuery::Change { .. } => "change",
                        SetupQuery::ReviewMemory { .. } => "review",
                        SetupQuery::Team { .. } => "team",
                        SetupQuery::TeamChange { .. } => "team change",
                        SetupQuery::TeamMessages { .. } => "team messages",
                        SetupQuery::Library(_) => "library",
                        SetupQuery::Log { .. } => "log",
                        SetupQuery::ReadPaths { .. } => "read paths",
                        SetupQuery::HiddenPaths { .. } => "hidden paths",
                        SetupQuery::AgentPolicy { .. } => "agent policy",
                        SetupQuery::CoordQuestions { .. } => "coord questions",
                        SetupQuery::OpenLiveSession { .. } => "live session open",
                        SetupQuery::MoveHome { .. } => "move home",
                    });
                    connection.project_setup(query).unwrap();
                }
                Request::Changes(query) => {
                    sent.push("changes");
                    connection.changes(*query).unwrap();
                }
                Request::Recovery(query) => {
                    sent.push("recovery");
                    connection.recovery(*query).unwrap();
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
                    sent.push(match query.as_ref() {
                        ProcessQuery::PendingImport { .. } => "pending import",
                        ProcessQuery::ConfirmImport { .. } => "confirm import",
                        ProcessQuery::Uncommitted { .. } => "uncommitted",
                        ProcessQuery::CommitImport { .. } => "commit import",
                        _ => "process",
                    });
                    connection.process(*query).unwrap();
                }
                Request::Questions(query)
                    if matches!(
                        *query,
                        crate::tui::client::questions::QuestionsQuery::Counts
                            | crate::tui::client::questions::QuestionsQuery::List { .. }
                    ) =>
                {
                    connection.questions(*query).unwrap();
                }
                Request::Roadmap(query) => connection.roadmap(*query).unwrap(),
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
        self.project_led_by(id, name, generation, None).await
    }

    async fn project_led_by(
        &mut self,
        id: &str,
        name: &str,
        generation: u64,
        team: Option<TeamPlan>,
    ) -> ProjectId {
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
                    team,
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
fn the_move_banner_names_the_checked_out_branch_merges_into_are_refused() {
    let files = ["CLAUDE.md".to_owned(), "AGENTS.md".to_owned()];
    let named = move_banner(&files, Some("trial4-land"), "…");
    assert!(
        named.starts_with("Git still shows CLAUDE.md, AGENTS.md as deleted after the instruction import, so merges into trial4-land are refused."),
        "{named}"
    );
    assert!(!named.contains("this branch"), "{named}");
    let detached = move_banner(&files[..1], None, "…");
    assert!(
        detached.contains("so merges into this branch are refused."),
        "a detached HEAD has no branch to name: {detached}"
    );
}

#[test]
fn the_move_banner_keeps_are_refused_within_its_rows_for_the_longest_branch_names() {
    let files: Vec<String> = [
        "CLAUDE.md",
        ".claude/CLAUDE.md",
        "AGENTS.md",
        "CLAUDE.local.md",
    ]
    .map(str::to_owned)
    .to_vec();
    let ascii = "a".repeat(crate::landing::BRANCH_BYTES_MAX);
    let wide = "\u{8a9e}".repeat(crate::landing::BRANCH_BYTES_MAX / 3);
    for branch in [&ascii, &wide] {
        assert!(branch.len() <= crate::landing::BRANCH_BYTES_MAX);
        let text = move_banner(&files, Some(branch), "…");
        for width in [78, 118] {
            let shown = crate::tui::text::wrap(&text, width)
                .into_iter()
                .take(BANNER_ROWS_MAX)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(shown.contains(" are refused."), "{width}: {shown}");
        }
    }
    let cut = move_banner(&files[..1], Some(&ascii), ".");
    assert!(
        cut.contains(". are refused.") && !cut.contains(&ascii),
        "a long name is cut with the ellipsis glyph: {cut}"
    );
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

/// A project made in the projects home keeps its context in `.crazyagent/context`: `o` on
/// each context file the engine listed opens that file, not a missing one beside the project.
#[test]
fn o_opens_each_listed_context_file_from_the_folder_the_engine_listed_it_in() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-ctx-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let project = root.path().canonicalize().unwrap().join("notes-cli");
    let state = project.join(".crazyagent");
    std::fs::create_dir_all(state.join("context/research")).unwrap();
    std::fs::write(state.join("context/brief.md"), "# Brief").unwrap();
    std::fs::write(state.join("context/research/market.md"), "# Market").unwrap();
    let id = ProjectId::new("notes-cli").unwrap();
    let stored = crate::project_setup::StoredSetup {
        project_id: id.clone(),
        name: "Notes CLI".into(),
        about: "Terminal-first notes.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Cli],
        research: vec![ResearchKind::CompetitorResearch],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: project.display().to_string(),
        created_ms: 1_793_000_000_000,
        revision: 1,
    };
    let context = crate::project_setup::list_context(&stored).unwrap();
    let paths: Vec<&str> = context
        .items
        .iter()
        .map(|item| item.path.as_str())
        .collect();
    assert_eq!(paths, ["context/brief.md", "context/research/market.md"]);
    let mut screen = super::project::ProjectScreen::new(id, "Notes CLI".into(), None);
    screen.setup = Some(stored);
    screen.context = Some(context.clone());

    let opened: Vec<_> = context
        .items
        .iter()
        .map(|item| {
            screen
                .context_request(&item.path)
                .map(|request| opening(&request))
        })
        .collect();

    assert_eq!(
        opened,
        vec![
            Some(Ok(Opening::Open(state.join("context/brief.md")))),
            Some(Ok(Opening::Open(state.join("context/research/market.md")))),
        ],
        "every listed context file opens from the project's .crazyagent folder"
    );
}

#[test]
fn a_changed_file_goes_to_the_editor_only_as_the_owners_copy_inside_the_project_folder() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-changed-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let home = root.path().canonicalize().unwrap();
    let folder = home.join("notes");
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("src/app.rs"), "fn main() {}\n").unwrap();
    std::fs::write(home.join("outside.rs"), "kept outside").unwrap();
    std::os::unix::fs::symlink(home.join("outside.rs"), folder.join("src/escape.rs")).unwrap();
    assert_eq!(
        changed_file(&folder, "src/app.rs"),
        Ok(folder.join("src/app.rs"))
    );
    assert_eq!(
        changed_file(&folder, "src/added.rs"),
        Err(
            "That file isn't in your project folder yet. It lands there when you accept the change."
        )
    );
    for refused in ["src/escape.rs", "../outside.rs", "src"] {
        assert_eq!(
            changed_file(&folder, refused),
            Err("CrazyAgent opens only files inside the project folder."),
            "{refused}"
        );
    }
    assert_eq!(
        changed_file(&home.join("gone"), "src/app.rs"),
        Err("That file or folder is not there any more.")
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
            "pending import",
            "uncommitted",
            "recovery",
            "setup",
            "team graph",
            "jobs",
            "context",
            "memories"
        ],
        "a project page also reads its instruction files waiting to import, its uncommitted move and its recovery notices"
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
    shell.key(Key::ShiftLeft);
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("This project has no pool, so the master cannot start."),
        "{shown}"
    );
    shell.key(Key::Char('h'));
    shell.key(Key::Enter);
    while let Some(request) = shell.ctx.next_request() {
        let read = match &request {
            Request::Process(query) => matches!(
                **query,
                ProcessQuery::PendingImport { .. } | ProcessQuery::Uncommitted { .. }
            ),
            Request::Questions(query) => matches!(
                **query,
                QuestionsQuery::Counts | QuestionsQuery::List { .. }
            ),
            other => matches!(
                other,
                Request::Pools
                    | Request::PoolProjects
                    | Request::Master(_)
                    | Request::Changes(_)
                    | Request::TeamGraph { .. }
                    | Request::Setup(
                        SetupQuery::Setup { .. }
                            | SetupQuery::Jobs { .. }
                            | SetupQuery::Log { .. }
                            | SetupQuery::Context { .. }
                            | SetupQuery::Memories { .. }
                            | SetupQuery::Team { .. }
                    )
            ),
        };
        assert!(read, "{request:?}");
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
        pool_id: None,
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

/// A master harness saved from the Team screen over the project page, as Team settings save
/// it, is read again once the owner is back on the page: the Conversation tab routes by the
/// new master, here an RCLI console instead of the live master view, its head names the new
/// master, and a General edit saves at the engine's revision instead of being refused as
/// changed (rv-code TP1-M1, rv-secreview).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_master_saved_from_the_team_screen_is_read_again_by_the_project_page() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let tab = |tab: ProjectTab| Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab,
    };
    let mut connection = engine.connect();
    let mut shell = shell_on(&tab(ProjectTab::Agents));
    pump(&mut shell, &mut connection).await;

    shell.key(Key::Char('t'));
    assert_eq!(shell.depth(), 2, "t opens the Team screen over the page");
    pump(&mut shell, &mut connection).await;
    let stored = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        stored.team.master, "rcli",
        "the project starts on a live master"
    );
    let generation = shell.ctx.next_generation();
    shell.ctx.request(Request::Setup(SetupQuery::Change {
        action: crate::project_setup::SetupAction::Update {
            project_id: notes.clone(),
            expected_revision: stored.revision,
            name: None,
            setup: ProjectSetup {
                about: stored.about.clone(),
                tech_stack: stored.tech_stack.clone(),
                platforms: stored.platforms.clone(),
                research: stored.research.clone(),
                pool: stored.pool.clone(),
                team: Some(TeamPlan {
                    master: "rcli".into(),
                    master_tools: true,
                    sub_harnesses: vec![],
                    min_agents: 1,
                    max_agents: 1,
                }),
                documents: Vec::new(),
            },
        },
        generation,
    }));
    pump(&mut shell, &mut connection).await;
    let saved = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved.revision > stored.revision && saved.team.master == "rcli",
        "the master saved from the Team screen landed"
    );

    shell.key(Key::Esc);
    assert_eq!(shell.depth(), 1, "Esc goes back to the page");
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    assert_eq!(
        shell.place(),
        Some(tab(ProjectTab::Conversation)),
        "an RCLI master keeps the console, not the live master view"
    );
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("Master  rcli"),
        "the console names the saved master:\n{shown}"
    );

    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    assert_eq!(shell.place(), Some(tab(ProjectTab::Settings)));
    shell.key(Key::Char('g'));
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Edit project"), "{shown}");
    shell.key(Key::Paste(" Pro".into()));
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"change"), "{sent:?}");
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Changes saved."), "{shown}");
    assert!(!shown.contains("This project changed"), "{shown}");
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
    shell.key(Key::ShiftLeft);
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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
    shell.key(Key::ShiftLeft);
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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
    shell.key(Key::ShiftLeft);
    until_place(
        &mut shell,
        &mut connection,
        &Place::Master {
            id: notes.clone(),
            name: "Notes CLI".into(),
        },
    )
    .await;
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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
        shown.contains("No messages yet.") && !shown.contains("Plan the first slice")
    })
    .await;
    assert!(!shown.contains("RCLI stopped"), "{shown}");
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    let fresh = fresh.unwrap();
    assert_ne!(fresh.session_id, first.session_id);
    assert_eq!(fresh.phase, MasterSessionPhase::Configured);

    say(&mut shell, &mut connection, "Plan the second slice").await;
    let second = settled_master(&mut engine, &notes, &mut shell, &mut connection, 1).await;
    assert_eq!(second.session_id, fresh.session_id);
    shell.key(Key::Esc);
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("1 earlier") && shown.contains("Plan the second slice")
    })
    .await;
    assert!(!shown.contains("No messages yet."), "{shown}");
    assert!(!shown.contains("Plan the first slice"), "{shown}");

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
        shown.contains("RCLI stopped")
            && shown.contains("Plan the first slice")
            && !shown.contains("Sessions  2")
    })
    .await;
    assert!(shown.contains("1 earlier"), "{shown}");
    assert!(!shown.contains("No messages yet."), "{shown}");
    assert!(!shown.contains("Plan the second slice"), "{shown}");
    let (resumed, _) = latest_master(&mut engine, &notes).await;
    let resumed = resumed.unwrap();
    assert_eq!(resumed.session_id, first.session_id);
    assert_eq!(resumed.phase, MasterSessionPhase::Configured);
}

/// n after another client changed the settings starts a new session on the current
/// settings instead of "The master could not start" with master_source_conflict (N-T2,
/// trial 4 run 5 step 60): the page read its catalog and the live session's binding before
/// the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_after_a_settings_change_on_another_client_starts_a_new_session() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    let pools = engine
        .controller
        .read(OwnerRead::ModelAccessPage {
            collection: crate::engine::owner::ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = &pools.data else {
        panic!("pool page required");
    };
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    engine
        .controller
        .begin_mutation(
            OwnerMutation::ModelAccess {
                expected_config_id: pools.config_id.clone(),
                expected_source_hash: source_hash.clone(),
                edit: crate::config::ModelAccessEdit::ReplacePool {
                    pool: crate::config::AccountPool {
                        id: reference("pool-b"),
                        name: Some("Spare notes".into()),
                        members: vec![reference("local-a")],
                        projects: vec![],
                        roles: vec![],
                        fallback_allowed: false,
                        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
                    },
                },
            },
            1,
        )
        .unwrap();
    let renamed = engine.controller.send_pending().await.unwrap();
    assert!(
        matches!(
            &renamed,
            OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
                if operation.result == crate::engine::owner::OwnerResult::Applied
        ),
        "another client changes the settings: {renamed:?}"
    );

    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Char('n'));
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("No messages yet.") && !shown.contains("Plan the first slice")
    })
    .await;
    assert!(!shown.contains("could not start"), "{shown}");
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    let fresh = fresh.unwrap();
    assert_ne!(fresh.session_id, first.session_id);
    assert_eq!(fresh.phase, MasterSessionPhase::Configured);
}

/// n whose read of the current settings fails says on the page that the new session was
/// not started and why, and starts none, instead of dropping it behind the generic metadata
/// line (NT2-L1): the read goes to another engine, which refuses it with
/// master_owner_identity_changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_whose_settings_read_fails_says_the_new_session_was_not_started() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    let other = Engine::start().await;
    let mut stranger = other.connect();
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Char('n'));
    let mut read = None;
    for _ in 0..shell.ctx.queued() {
        match shell.ctx.next_request().unwrap() {
            Request::Master(job) if matches!(*job, MasterJob::Catalog { .. }) => read = Some(job),
            other => shell.ctx.request(other),
        }
    }
    let read = read.expect("n reads the current settings first");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut asked = false;
    let event = loop {
        assert!(Instant::now() < deadline, "the other engine never answered");
        match stranger.poll_owner().unwrap() {
            Some(OwnerEvent::Access { ready: true, .. }) if !asked => {
                stranger.master((*read).clone()).unwrap();
                asked = true;
            }
            Some(OwnerEvent::Master(event)) => break event,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    };
    assert!(
        matches!(
            event.as_ref(),
            MasterEvent::Catalog { result: Err(problem), .. }
                if problem == "master_owner_identity_changed"
        ),
        "the other engine refuses the read"
    );
    shell.arrive(owner_arrival(OwnerEvent::Master(event)));
    let message = shell
        .ctx
        .message()
        .cloned()
        .expect("the page says what happened");
    assert_eq!(message.tone, Tone::Warning);
    assert_eq!(message.lead, "New session not started.");
    assert_eq!(
        message.rest,
        "The current settings could not be read (master_owner_identity_changed). \
         Press n to try again."
    );
    pump(&mut shell, &mut connection).await;
    let (latest, _) = latest_master(&mut engine, &notes).await;
    assert_eq!(
        latest.unwrap().session_id,
        first.session_id,
        "no new session was started"
    );
}

/// A second n while the first New session still starts says so in plain words on the
/// page, not the client's internal intent line, and only one new session is made (P2,
/// trial 4 run 6 N-T2): the first n is still reading the current settings when the
/// second arrives. Once the new session lands, the wait is over and the error is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_n_while_the_first_new_session_starts_says_it_is_already_starting() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Char('n'));
    shell.key(Key::Char('n'));
    let message = shell
        .ctx
        .message()
        .cloned()
        .expect("the second n is answered on the page");
    assert_eq!(message.tone, Tone::Error);
    assert_eq!(message.lead, "The session did not switch.");
    assert_eq!(message.rest, "A new session is already starting.");
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("A new session is already starting."),
        "{shown}"
    );
    for internal in ["intent", "exact identity", "Ctrl-R recovers"] {
        assert!(!shown.contains(internal), "{internal}:\n{shown}");
    }
    until(&mut shell, &mut connection, |shown| {
        shown.contains("No messages yet.") && !shown.contains("Plan the first slice")
    })
    .await;
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    assert_ne!(fresh.unwrap().session_id, first.session_id);
    assert_eq!(
        master_sessions(&engine, &notes),
        2,
        "one new session, not two"
    );
    assert_eq!(
        shell.ctx.message(),
        None,
        "the wait ended, so its error is gone"
    );
    let shown = render(&mut shell, 100, 40);
    assert!(
        !shown.contains("A new session is already starting."),
        "{shown}"
    );
}

/// A second n once the first New session has its settings and waits on the engine says
/// the same plain words (P2): the settings read is answered by hand, so the New session
/// is queued for the engine and not yet made when the second n arrives. An error said
/// after it stays when the new session lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_n_while_the_engine_makes_the_first_new_session_says_it_is_already_starting() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Char('n'));
    let mut read = None;
    for _ in 0..shell.ctx.queued() {
        match shell.ctx.next_request().unwrap() {
            Request::Master(job) if matches!(*job, MasterJob::Catalog { .. }) => read = Some(job),
            other => shell.ctx.request(other),
        }
    }
    connection
        .master(*read.expect("n reads the current settings first"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < deadline,
            "the settings read was never answered"
        );
        match connection.poll_owner().unwrap() {
            Some(OwnerEvent::Master(event))
                if matches!(event.as_ref(), MasterEvent::Catalog { .. }) =>
            {
                shell.arrive(owner_arrival(OwnerEvent::Master(event)));
                break;
            }
            Some(event) => shell.arrive(owner_arrival(event)),
            None => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    shell.key(Key::Char('n'));
    let message = shell
        .ctx
        .message()
        .cloned()
        .expect("the second n is answered on the page");
    assert_eq!(message.lead, "The session did not switch.");
    assert_eq!(message.rest, "A new session is already starting.");
    let other = Message::error("The master's turn did not stop.", "It had already ended.");
    shell.ctx.say(other.clone());
    until(&mut shell, &mut connection, |shown| {
        shown.contains("No messages yet.") && !shown.contains("Plan the first slice")
    })
    .await;
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    assert_ne!(fresh.unwrap().session_id, first.session_id);
    assert_eq!(
        master_sessions(&engine, &notes),
        2,
        "one new session, not two"
    );
    assert_eq!(
        shell.ctx.message(),
        Some(&other),
        "an error said after the wait stays"
    );
}

/// n while another master step is still under way, here a message just sent and not yet
/// taken by the engine, says so in plain words, not the client's internal intent line
/// (P2 r1, TUIP2-L1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_while_a_sent_message_is_under_way_says_so_in_plain_words() {
    let mut engine = Engine::start().await;
    let (_, mut shell, mut connection, _) = conversation_with_a_settled_turn(&mut engine).await;
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Enter);
    for character in "Plan the second slice".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    shell.key(Key::Char('n'));
    let message = shell
        .ctx
        .message()
        .cloned()
        .expect("n is answered on the page");
    assert_eq!(message.tone, Tone::Error);
    assert_eq!(message.lead, "The session did not switch.");
    assert_eq!(
        message.rest,
        "Your last request is still being handled. Press n again when it is done."
    );
    let shown = render(&mut shell, 100, 40);
    for internal in ["intent", "exact identity", "Ctrl-R recovers"] {
        assert!(!shown.contains(internal), "{internal}:\n{shown}");
    }
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

/// A new session asked for while no session is in view turns the live switch on at once,
/// since there is no earlier session the live master could start on, and the engine takes
/// the change.
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
    for _ in 0..5 {
        shell.key(Key::ShiftLeft);
    }
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
    pump(&mut shell, &mut connection).await;
    let (none, _) = latest_master(&mut engine, &notes).await;
    assert!(none.is_none(), "the project has no master session yet");
    let before = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_ne!(before.live_master, Some(true), "the switch starts off");

    shell.key(Key::Esc);
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
        pool_id: None,
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
    for _ in 0..5 {
        shell.key(Key::ShiftLeft);
    }
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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

    for _ in 0..5 {
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
        pool_id: None,
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
    for _ in 0..5 {
        shell.key(Key::ShiftLeft);
    }
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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

/// While the owner types in the Conversation composer, Ctrl-D stays with the composer and
/// the banner's notice is not dismissed; once the owner leaves the composer, Ctrl-D
/// dismisses it.
#[test]
fn ctrl_d_while_typing_in_the_conversation_never_dismisses_a_recovery_notice() {
    use crate::engine::owner::{BannerNotice, NoticeShown, RecoveryBanner};
    use crate::tui::client::recovery::{RecoveryEvent, RecoveryQuery, RecoveryReply};
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
    let recovery = |shell: &mut Shell| {
        let mut asked = Vec::new();
        while let Some(request) = shell.ctx.next_request() {
            if let Request::Recovery(query) = request {
                asked.push(*query);
            }
        }
        asked
    };
    let read = recovery(&mut shell);
    assert_eq!(
        read,
        [RecoveryQuery::Read {
            project: notes.clone()
        }]
    );
    let banner = RecoveryBanner {
        runs: Vec::new(),
        notices: vec![BannerNotice {
            action_kind: crate::ledger::ActionKind::AgentSpawn,
            id: 7,
            agent: Some(2),
            label: Some("Ada".into()),
            change_set: None,
            shown: NoticeShown::Unknown,
            started_ms: 1_800_000_000_000,
        }],
        more: 0,
    };
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Recovery(Box::new(
        RecoveryEvent {
            query: read[0].clone(),
            result: Ok(RecoveryReply::Banner(banner)),
        },
    )))));

    shell.key(Key::Ctrl('d'));
    assert_eq!(
        recovery(&mut shell),
        Vec::<RecoveryQuery>::new(),
        "Ctrl-D while typing is the composer's"
    );

    shell.key(Key::Tab);
    shell.key(Key::Ctrl('d'));
    assert_eq!(
        recovery(&mut shell),
        [RecoveryQuery::Dismiss {
            project: notes,
            kind: crate::ledger::ActionKind::AgentSpawn,
            id: 7
        }],
        "out of the composer, Ctrl-D dismisses the notice"
    );
}

#[test]
fn the_recovery_banner_on_the_conversation_tab_says_tab_first_while_the_composer_has_the_keys() {
    use crate::engine::owner::{BannerNotice, NoticeShown, RecoveryBanner};
    use crate::tui::client::recovery::{RecoveryEvent, RecoveryQuery, RecoveryReply};
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
    let mut read = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Recovery(query) = request {
            read.push(*query);
        }
    }
    assert_eq!(read, [RecoveryQuery::Read { project: notes }]);
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Recovery(Box::new(
        RecoveryEvent {
            query: read[0].clone(),
            result: Ok(RecoveryReply::Banner(RecoveryBanner {
                runs: Vec::new(),
                notices: vec![BannerNotice {
                    action_kind: crate::ledger::ActionKind::AgentSpawn,
                    id: 7,
                    agent: Some(2),
                    label: Some("Ada".into()),
                    change_set: None,
                    shown: NoticeShown::Unknown,
                    started_ms: 1_800_000_000_000,
                }],
                more: 0,
            })),
        },
    )))));

    let typing = render(&mut shell, 80, 24);
    assert!(
        typing.contains("Tab first, then Ctrl-D Dismiss Ada's start"),
        "the composer has Ctrl-D, so the banner says Tab first:\n{typing}"
    );

    shell.key(Key::Tab);
    let out = render(&mut shell, 80, 24);
    assert!(
        out.contains("Ctrl-D Dismiss Ada's start") && !out.contains("Tab first"),
        "out of the composer the key works as shown:\n{out}"
    );
}

#[test]
fn at_eighty_columns_with_a_failed_run_and_a_notice_every_banner_key_is_drawn_whole() {
    use crate::engine::owner::{BannerNotice, BannerRun, NoticeShown, RecoveryBanner, RunShown};
    use crate::tui::client::recovery::{RecoveryEvent, RecoveryQuery, RecoveryReply};
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
    let mut read = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Recovery(query) = request {
            read.push(*query);
        }
    }
    assert_eq!(read, [RecoveryQuery::Read { project: notes }]);
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Recovery(Box::new(
        RecoveryEvent {
            query: read[0].clone(),
            result: Ok(RecoveryReply::Banner(RecoveryBanner {
                runs: vec![BannerRun {
                    agent: 3,
                    label: "Sub-agent 3".into(),
                    role: crate::team::AgentRole::Sub,
                    harness: "Codex".into(),
                    at_ms: 1_800_000_000_000,
                    turn: 3,
                    asked: Some("Plan the page".into()),
                    state: RunShown::Failed,
                    reason: Some("The account is unavailable.".into()),
                    paused: false,
                }],
                notices: vec![BannerNotice {
                    action_kind: crate::ledger::ActionKind::AgentSpawn,
                    id: 7,
                    agent: Some(2),
                    label: Some("Ada".into()),
                    change_set: None,
                    shown: NoticeShown::Unknown,
                    started_ms: 1_800_000_000_000,
                }],
                more: 0,
            })),
        },
    )))));

    for (state, out) in [
        ("typing", render(&mut shell, 80, 24)),
        ("after Tab", {
            shell.key(Key::Tab);
            render(&mut shell, 80, 24)
        }),
    ] {
        for key in [
            "Ctrl-R Retry Sub-agent 3",
            "Ctrl-D Dismiss Ada's start",
            "Ctrl-N Next item",
        ] {
            assert!(
                out.lines().any(|line| line.contains(key)),
                "{state}: {key:?} is drawn whole on one line:\n{out}"
            );
        }
        assert!(
            !out.lines()
                .any(|line| line.contains("Ctrl-") && line.contains('\u{2026}')),
            "{state}: no key line is cut:\n{out}"
        );
    }
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
    leave_the_master_view(&mut shell, &notes, "Notes CLI");
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

fn master_sessions(engine: &Engine, project: &ProjectId) -> i64 {
    let connection = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM master_sessions WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap()
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

/// The Team settings screen a project opens with s (TUI-REACH-1) runs the master harness
/// picker as the Team screen does (OP-28): H reads the harness catalog and the stored setup,
/// and Enter on the next harness saves it in the engine through the project setup.
#[tokio::test]
async fn the_team_settings_screen_reads_the_harness_catalog_and_saves_the_picked_master() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let before = engine
        .controller
        .project_setup(notes.clone())
        .await
        .unwrap()
        .unwrap();
    let mut connection = engine.connect();
    let mut ctx = ctx(200, 40);
    let screen = open(
        &Place::TeamSettings {
            id: notes.clone(),
            name: "Notes CLI".into(),
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;

    shell.key(Key::Char('H'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"harnesses"), "{sent:?}");
    assert!(sent.contains(&"setup"), "{sent:?}");
    let shown = render(&mut shell, 200, 40);
    assert!(shown.contains("Harness for the master"), "{shown}");
    assert!(!shown.contains("Reading the harnesses"), "{shown}");

    shell.key(Key::Down);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"change"), "{sent:?}");
    let after = engine
        .controller
        .project_setup(notes)
        .await
        .unwrap()
        .unwrap();
    assert!(
        after.revision > before.revision && after.team.master != before.team.master,
        "the picked master landed in the project setup: {} then {}",
        before.team.master,
        after.team.master
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

    let version = engine
        .controller
        .plugin_servers(None)
        .await
        .unwrap()
        .into_iter()
        .find(|view| view.id == server.id)
        .unwrap()
        .version;
    engine
        .controller
        .remove_mcp_server(server.id.clone(), version)
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

/// PROTO-4 r1 F3: an account order save for a project the engine no longer knows is refused
/// by the real engine, and that reply reaches the project page as the plain sentence, never
/// the code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_account_order_for_a_gone_project_reaches_the_page_as_its_sentence() {
    use crate::scheduling::{ScheduleChange, Strategy};
    use crate::tui::client::settings::SettingsEvent;
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    owner_ready(&mut connection).await;
    let SettingsEvent::Schedule(Ok(schedule)) =
        settings_reply(&mut connection, SettingsJob::Schedule).await
    else {
        panic!("a schedule reply expected");
    };
    owner_ready_after(&mut connection).await;
    let refused = settings_reply(
        &mut connection,
        SettingsJob::SetSchedule {
            change: ScheduleChange::ProjectStrategy {
                project: CredentialRef::new("project-gone").unwrap(),
                strategy: Some(Strategy::Balanced),
            },
            version: schedule.version,
            generation: 2,
        },
    )
    .await;
    let SettingsEvent::ScheduleSet(_, Err(code)) = &refused else {
        panic!("a refused save expected");
    };
    assert_eq!(code, "project_not_found");
    let mut ctx = ctx(80, 24);
    ctx.link = crate::tui::ctx::Link::Ready;
    let screen = open(
        &Place::Project {
            id: notes,
            name: "Notes CLI".into(),
            tab: ProjectTab::Conversation,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    while shell.ctx.next_request().is_some() {}
    shell.arrive(owner_arrival(OwnerEvent::Settings(Box::new(refused))));
    let message = shell
        .ctx
        .message()
        .cloned()
        .expect("the page says why the save failed");
    assert_eq!(message.tone, Tone::Error);
    assert_eq!(
        message.lead,
        "This project no longer exists. Refresh and try again."
    );
}

async fn owner_ready_after(connection: &mut Connection) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while connection.busy() {
        while connection.poll_owner().unwrap().is_some() {}
        assert!(Instant::now() < deadline, "the owner job never finished");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The top border of the box that lists moved instruction files, as the modal draws its title.
const MOVED_BOX: &str = "─ Moved to .crazyagent/imported/ ─";

/// The one project folder the engine made under its projects home.
fn only_folder(engine: &Engine) -> std::path::PathBuf {
    let folders: Vec<std::path::PathBuf> = std::fs::read_dir(engine.home.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(folders.len(), 1, "{folders:?}");
    folders[0].canonicalize().unwrap()
}

/// Opens the project page on Overview against the real engine and serves its first reads.
async fn opened_page(
    project: &ProjectId,
    connection: &mut Connection,
) -> (Shell, Vec<&'static str>) {
    let mut ctx = ctx(100, 40);
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, connection).await;
    (shell, sent)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_import_prompt_moves_files_on_confirm_and_asks_again_after_not_now() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let folder = only_folder(&engine);
    crate::tui::test_home::instruction_repo(&folder, &["CLAUDE.md"], &["AGENTS.md"]);
    let mut connection = engine.connect();

    let (mut shell, sent) = opened_page(&notes, &mut connection).await;
    assert!(sent.contains(&"pending import"), "{sent:?}");
    let shown = render(&mut shell, 100, 40);
    let flat = shown
        .lines()
        .map(|row| row.trim_matches(|c: char| c == '\u{2502}' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    for want in [
        "Instruction files found",
        "- CLAUDE.md (15 bytes, tracked by git)",
        "- AGENTS.md (15 bytes)",
        "Git tracks some of them, so after the move git shows them as deleted.",
        "Enter Copy into instructions.md and move aside",
        "Esc Not now",
    ] {
        assert!(flat.contains(want), "{want} missing in\n{shown}");
    }
    assert!(!shown.contains("AGENTS.md (15 bytes, tracked"), "{shown}");
    shell.key(Key::Esc);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"confirm import"), "{sent:?}");
    assert!(!render(&mut shell, 100, 40).contains("Instruction files found"));
    shell.refresh();
    pump(&mut shell, &mut connection).await;
    assert!(
        !render(&mut shell, 100, 40).contains("Instruction files found"),
        "Not now holds for this opening"
    );
    assert!(folder.join("CLAUDE.md").is_file() && folder.join("AGENTS.md").is_file());

    let mut connection = engine.connect();
    let (mut shell, sent) = opened_page(&notes, &mut connection).await;
    assert!(sent.contains(&"pending import"), "{sent:?}");
    assert!(
        render(&mut shell, 100, 40).contains("Instruction files found"),
        "the next opening asks again"
    );
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"confirm import"), "{sent:?}");
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains("Instruction files found"), "{shown}");
    assert!(
        shown.contains(MOVED_BOX),
        "the moved files show in their own box\n{shown}"
    );
    assert!(
        shown.contains("- CLAUDE.md (git shows it as deleted)"),
        "{shown}"
    );
    assert!(shown.contains("- AGENTS.md"), "{shown}");
    let imported = folder.join(".crazyagent/imported");
    assert!(imported.join("CLAUDE.md").is_file() && imported.join("AGENTS.md").is_file());
    assert!(!folder.join("CLAUDE.md").exists() && !folder.join("AGENTS.md").exists());
    let instructions = std::fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap();
    assert!(
        instructions.contains("CLAUDE.md rule") && instructions.contains("AGENTS.md rule"),
        "{instructions}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untracked_only_move_shows_message_and_no_banner() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let folder = only_folder(&engine);
    let tip = crate::tui::test_home::instruction_repo(&folder, &[], &["AGENTS.md"]);
    let mut connection = engine.connect();
    let (mut shell, _) = opened_page(&notes, &mut connection).await;
    assert!(render(&mut shell, 100, 40).contains("- AGENTS.md (15 bytes)"));
    shell.key(Key::Char('y'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"confirm import"), "{sent:?}");
    assert!(sent.contains(&"uncommitted"), "{sent:?}");
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Moved to .crazyagent/imported/:");
    assert_eq!(message.rest, "AGENTS.md");
    let shown = render(&mut shell, 100, 40);
    assert!(
        !shown.contains(MOVED_BOX),
        "an untracked-only move says so in the message line, not a box\n{shown}"
    );
    assert!(!shown.contains("Commit the move"), "{shown}");
    assert!(!shown.contains("Git still shows"), "{shown}");
    assert_eq!(
        crate::tui::test_home::git(&folder, &["rev-parse", "HEAD"]).trim(),
        tip
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_only_import_offers_no_confirm() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let folder = only_folder(&engine);
    let outside = engine.root.path().join("outside.md");
    std::fs::write(&outside, "kept outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("CLAUDE.md")).unwrap();
    let mut connection = engine.connect();
    let (mut shell, _) = opened_page(&notes, &mut connection).await;
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("- CLAUDE.md: stays in place, it links outside the project folder"),
        "{shown}"
    );
    assert!(
        shown.contains("None of them can be moved. Not now asks again the next time you"),
        "{shown}"
    );
    assert!(!shown.contains("Enter Copy into"), "{shown}");
    assert!(shown.contains("Esc Not now"), "{shown}");
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"confirm import"), "{sent:?}");
    assert!(
        std::fs::symlink_metadata(folder.join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!folder.join(".crazyagent/imported").exists());
}

/// Opens the page on a checkout with a tracked CLAUDE.md, confirms the import and leaves
/// the move uncommitted, so the banner shows. Output: the shell, connection, folder and
/// the tip before the import.
async fn left_move(engine: &mut Engine) -> (Shell, Connection, std::path::PathBuf, String) {
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let folder = only_folder(engine);
    let tip = crate::tui::test_home::instruction_repo(&folder, &["CLAUDE.md"], &[]);
    let mut connection = engine.connect();
    let (mut shell, _) = opened_page(&notes, &mut connection).await;
    shell.key(Key::Enter);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Moved to .crazyagent/imported/"), "{shown}");
    assert!(shown.contains("Enter Commit the move"), "{shown}");
    assert!(shown.contains("Esc Leave it"), "{shown}");
    shell.key(Key::Esc);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"uncommitted"), "{sent:?}");
    (shell, connection, folder, tip)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_with_tracked_file_offers_commit_then_banner_after_leave() {
    let mut engine = Engine::start().await;
    let (mut shell, mut connection, folder, tip) = left_move(&mut engine).await;
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains("Esc Leave it"), "{shown}");
    assert!(
        shown.contains("Git still shows CLAUDE.md as deleted after the instruction import"),
        "{shown}"
    );
    let checked_out = crate::tui::test_home::git(&folder, &["branch", "--show-current"]);
    let words = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains(&format!(
            "so merges into {} are refused.",
            checked_out.trim()
        )),
        "the banner names the checked-out branch\n{shown}"
    );
    let hints = shown.lines().last().unwrap().to_owned();
    assert!(
        hints.trim_start().starts_with("Ctrl-G Commit the move"),
        "{hints}"
    );
    assert_eq!(
        crate::tui::test_home::git(&folder, &["rev-parse", "HEAD"]).trim(),
        tip,
        "Leave it commits nothing"
    );

    let branch = crate::tui::test_home::git(&folder, &["branch", "--show-current"]);
    shell.key(Key::Ctrl('g'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"commit import"), "{sent:?}");
    let message = shell.ctx.message().unwrap();
    assert_eq!(
        message.lead,
        format!("Committed the move on {}:", branch.trim())
    );
    assert_eq!(message.rest, "CLAUDE.md. Nothing was pushed.");
    let git = |args: &[&str]| crate::tui::test_home::git(&folder, args);
    assert_eq!(git(&["rev-parse", "HEAD~1"]).trim(), tip);
    assert_eq!(
        git(&["show", "--name-status", "--format=", "HEAD"]).trim(),
        "D\tCLAUDE.md"
    );
    assert_eq!(
        git(&["log", "-1", "--format=%an <%ae>"]).trim(),
        "Owner <owner@example.invalid>"
    );
    assert_eq!(git(&["status", "--porcelain", "--", "CLAUDE.md"]), "");
    assert_eq!(git(&["remote"]), "", "nothing to push to, nothing pushed");
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains("Git still shows"), "{shown}");
    assert!(!shown.contains("Ctrl-G Commit the move"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_refusal_keeps_offer() {
    let mut engine = Engine::start().await;
    let (mut shell, mut connection, folder, tip) = left_move(&mut engine).await;
    std::fs::write(folder.join("other.txt"), "staged\n").unwrap();
    crate::tui::test_home::git(&folder, &["add", "other.txt"]);
    shell.key(Key::Ctrl('g'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"commit import"), "{sent:?}");
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        super::process::message("process_commit_staged")
    );
    assert_eq!(
        crate::tui::test_home::git(&folder, &["rev-parse", "HEAD"]).trim(),
        tip,
        "a refusal commits nothing"
    );
    assert_eq!(
        crate::tui::test_home::git(&folder, &["diff", "--cached", "--name-only"]).trim(),
        "other.txt",
        "the owner's staged file stays staged and alone"
    );
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("Git still shows CLAUDE.md as deleted"),
        "the banner stays: {shown}"
    );
    assert!(shown.contains("Ctrl-G Commit the move"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s_on_the_agents_tab_opens_team_settings_and_each_setting_reaches_the_engine() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(120, 30);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Agents,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("s Team settings"), "{shown}");

    shell.key(Key::Char('s'));
    assert_eq!(shell.depth(), 2);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"team"), "{sent:?}");
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("Notes CLI  /  Team settings"), "{shown}");
    assert!(shown.contains("Keep warm  Off"), "{shown}");
    assert!(shown.contains("t Task limit"), "{shown}");

    for key in ['t', 'l', 'i', 'h'] {
        shell.key(Key::Char(key));
        let sent = pump(&mut shell, &mut connection).await;
        assert_eq!(sent.first(), Some(&"team change"), "{key}: {sent:?}");
        assert!(
            sent.contains(&"team"),
            "{key} reads the team again: {sent:?}"
        );
    }
    let status = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_eq!(
        (
            status.turn_limit_secs,
            status.session_limit_secs,
            status.idle_stop_secs,
            status.on_harness_switch,
        ),
        (
            Some(1800),
            Some(1800),
            Some(3600),
            crate::team::SwitchMode::Fresh
        )
    );
    let shown = render(&mut shell, 120, 30);
    assert!(
        shown.contains("When the master harness changes, the new harness starts fresh."),
        "{shown}"
    );

    shell.key(Key::Esc);
    assert_eq!(shell.depth(), 1);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes,
            name: "Notes CLI".into(),
            tab: ProjectTab::Agents,
        })
    );
}

fn git_in(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn pending_change(
    root: &std::path::Path,
    home: &std::path::Path,
    repo: &std::path::Path,
    project: &ProjectId,
    name: &str,
) -> crate::landing::ChangeSetId {
    use crate::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
    let worktree = repo.parent().unwrap().join(name);
    git_in(
        home,
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let db = crate::landing::open(root).unwrap();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',1,'stopped',1,1)",
        [project.as_str()],
    )
    .unwrap();
    let member = db.last_insert_rowid();
    let git_dir = repo.join(".git/worktrees").join(name);
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
         VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            member,
            project.as_str(),
            repo.to_str().unwrap(),
            worktree.to_str().unwrap(),
            git_dir.to_str().unwrap()
        ],
    )
    .unwrap();
    std::fs::write(worktree.join("README"), format!("one\n{name}\n")).unwrap();
    let workspace = Workspace {
        project: project.clone(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: member.to_string(),
            label: name.into(),
        },
        repository: repo.to_path_buf(),
        worktree,
        git_dir,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: format!("Work as {name}"),
    };
    crate::landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .expect("an edited worktree makes a change set")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_line_counts_the_change_sets_that_wait_for_the_owner() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let work = tempfile::Builder::new()
        .prefix("ca-tui-wait-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = std::fs::canonicalize(work.path()).unwrap();
    let (repo, home) = (base.join("repo"), base.join("home"));
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&home).unwrap();
    let owner = |args: &[&str]| git_in(&home, &repo, args);
    owner(&["init", "-q", "--template=", "-b", "main"]);
    owner(&["config", "user.name", "Repo Owner"]);
    owner(&["config", "user.email", "owner@example.invalid"]);
    owner(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README"), "one\n").unwrap();
    owner(&["add", "README"]);
    owner(&["commit", "-q", "-m", "Start"]);
    let root = engine.root.path().to_path_buf();
    pending_change(&root, &home, &repo, &notes, "agent-1");
    pending_change(&root, &home, &repo, &notes, "agent-2");
    let removed = ProjectId::new("removed-project").unwrap();
    pending_change(&root, &home, &repo, &removed, "agent-3");

    let mut connection = engine.connect();
    let mut ctx = ctx(120, 30);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Agents,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let header = |shell: &mut Shell| render(shell, 120, 30).lines().next().unwrap().to_owned();
    shell.tick();
    pump(&mut shell, &mut connection).await;
    let line = header(&mut shell);
    assert!(line.contains("2 change sets wait in Changes"), "{line}");

    let sets = engine.controller.change_sets(notes.clone()).await.unwrap();
    assert_eq!(sets.len(), 2);
    engine
        .controller
        .discard_changes(sets[0].id.clone(), sets[0].head.clone())
        .await
        .unwrap();
    shell.tick();
    pump(&mut shell, &mut connection).await;
    let line = header(&mut shell);
    assert!(line.contains("1 change set waits in Changes"), "{line}");

    engine
        .controller
        .discard_changes(sets[1].id.clone(), sets[1].head.clone())
        .await
        .unwrap();
    shell.tick();
    pump(&mut shell, &mut connection).await;
    let line = header(&mut shell);
    assert!(!line.contains("in Changes"), "{line}");
}

async fn waiting_header(engine: &Engine, place: Place) -> String {
    let mut connection = engine.connect();
    let mut ctx = ctx(160, 30);
    let screen = open(&place, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.tick();
    pump(&mut shell, &mut connection).await;
    render(&mut shell, 160, 30)
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_page_counts_its_own_change_sets_and_the_rest_as_other_projects() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let todo = engine.project("todo-app", "Todo App", 2).await;
    let work = tempfile::Builder::new()
        .prefix("ca-tui-here-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = std::fs::canonicalize(work.path()).unwrap();
    let (repo, home) = (base.join("repo"), base.join("home"));
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&home).unwrap();
    let owner = |args: &[&str]| git_in(&home, &repo, args);
    owner(&["init", "-q", "--template=", "-b", "main"]);
    owner(&["config", "user.name", "Repo Owner"]);
    owner(&["config", "user.email", "owner@example.invalid"]);
    owner(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README"), "one\n").unwrap();
    owner(&["add", "README"]);
    owner(&["commit", "-q", "-m", "Start"]);
    let root = engine.root.path().to_path_buf();
    pending_change(&root, &home, &repo, &notes, "agent-1");
    pending_change(&root, &home, &repo, &notes, "agent-2");
    pending_change(&root, &home, &repo, &todo, "agent-3");
    let removed = ProjectId::new("removed-project").unwrap();
    pending_change(&root, &home, &repo, &removed, "agent-4");
    let notes_page = || Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab: ProjectTab::Agents,
    };
    let todo_page = || Place::Project {
        id: todo.clone(),
        name: "Todo App".into(),
        tab: ProjectTab::Agents,
    };

    let line = waiting_header(&engine, notes_page()).await;
    assert!(
        line.contains("2 change sets wait in Changes, 1 in other projects"),
        "{line}"
    );
    let line = waiting_header(&engine, todo_page()).await;
    assert!(
        line.contains("1 change set waits in Changes, 2 in other projects"),
        "{line}"
    );
    let line = waiting_header(&engine, Place::Home).await;
    assert!(
        line.contains("3 change sets wait in your projects"),
        "{line}"
    );
    assert!(!line.contains("in Changes"), "{line}");
    let line = waiting_header(
        &engine,
        Place::Team {
            id: notes.clone(),
            name: "Notes CLI".into(),
        },
    )
    .await;
    assert!(
        line.contains("2 change sets wait in Changes, 1 in other projects"),
        "{line}"
    );

    let set = engine.controller.change_sets(todo.clone()).await.unwrap();
    assert_eq!(set.len(), 1);
    engine
        .controller
        .discard_changes(set[0].id.clone(), set[0].head.clone())
        .await
        .unwrap();
    let line = waiting_header(&engine, notes_page()).await;
    assert!(line.contains("2 change sets wait in Changes"), "{line}");
    assert!(!line.contains("other projects"), "{line}");
    let line = waiting_header(&engine, todo_page()).await;
    assert!(
        line.contains("2 change sets wait in other projects"),
        "{line}"
    );
    assert!(!line.contains("in Changes"), "{line}");
}

async fn desktop_time_limits(engine: &Engine, project: &ProjectId, session_secs: u32) {
    let client = Client::new(
        engine.root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut desktop) = OwnerController::attach_configured(client, OwnerClientKind::Desktop)
        .await
        .unwrap();
    let read = desktop.team_status(project.clone()).await.unwrap();
    let action = crate::team::TeamAction::TimeLimits {
        project_id: project.clone(),
        turn_secs: read.turn_limit_secs,
        session_secs: Some(session_secs),
        idle_secs: read.idle_stop_secs,
        version: read.settings_version,
    };
    match desktop.team_action(action, 1).await.unwrap() {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(
                operation.result,
                crate::engine::owner::OwnerResult::Applied,
                "the desktop change from its own read applies"
            )
        }
        other => panic!("desktop team change not applied: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tui_team_setting_from_before_a_desktop_change_is_refused_and_the_desktop_value_stays() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut ctx = ctx(120, 30);
    let screen = open(
        &Place::TeamSettings {
            id: notes.clone(),
            name: "Notes CLI".into(),
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("t Task limit"), "{shown}");

    desktop_time_limits(&engine, &notes, 7200).await;

    shell.key(Key::Char('t'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"team change"), "{sent:?}");
    let status = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_eq!(
        (status.turn_limit_secs, status.session_limit_secs),
        (None, Some(7200)),
        "the TUI change from the old read must not revert the desktop session limit"
    );
    assert!(
        sent.contains(&"team"),
        "a refused change reads the team again: {sent:?}"
    );
    let shown = render(&mut shell, 120, 30);
    assert!(
        shown.contains("This changed in another window, so yours was not saved."),
        "{shown}"
    );

    shell.key(Key::Char('t'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"team change"), "{sent:?}");
    let status = engine.controller.team_status(notes.clone()).await.unwrap();
    assert_eq!(
        (status.turn_limit_secs, status.session_limit_secs),
        (Some(1800), Some(7200)),
        "a change from the fresh read applies and keeps the desktop session limit"
    );
}

async fn team_settings_after(keys: &[char]) -> (crate::team::TeamStatus, String) {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
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
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('s'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"team"), "{sent:?}");
    for key in keys {
        shell.key(Key::Char(*key));
        let sent = pump(&mut shell, &mut connection).await;
        assert_eq!(sent.first(), Some(&"team change"), "{key}: {sent:?}");
        assert!(
            sent.contains(&"team"),
            "{key} reads the team again: {sent:?}"
        );
    }
    let status = engine.controller.team_status(notes).await.unwrap();
    (status, render(&mut shell, 80, 24))
}

fn shows_once(shown: &str, setting: &str) {
    assert_eq!(
        shown.lines().filter(|row| row.contains(setting)).count(),
        1,
        "{setting:?} shows once, whole, at 80x24:\n{shown}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_show_every_setting_whole_at_80_by_24() {
    let (status, shown) = team_settings_after(&[]).await;
    assert_eq!(
        (
            status.keep_warm,
            status.turn_limit_secs,
            status.session_limit_secs,
            status.idle_stop_secs,
            status.on_harness_switch,
        ),
        (false, None, None, Some(600), crate::team::SwitchMode::Carry)
    );
    for setting in [
        "Keep warm  Off",
        "Task  Off",
        "Session  Off",
        "Idle stop  10 min",
        "On harness switch  Carry over",
    ] {
        shows_once(&shown, setting);
    }
    assert!(
        shown.lines().all(|row| row.chars().count() <= 80),
        "{shown}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_turn_keep_warm_on() {
    let (status, shown) = team_settings_after(&['w']).await;
    assert!(status.keep_warm);
    shows_once(&shown, "Keep warm  On");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_turn_keep_warm_off_again() {
    let (status, shown) = team_settings_after(&['w', 'w']).await;
    assert!(!status.keep_warm);
    shows_once(&shown, "Keep warm  Off");
}

async fn task_limit_after(presses: usize, secs: Option<u32>, label: &str) {
    let (status, shown) = team_settings_after(&vec!['t'; presses]).await;
    assert_eq!(
        (status.turn_limit_secs, status.session_limit_secs),
        (secs, None)
    );
    shows_once(&shown, &format!("Task  {label}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_to_30_min() {
    task_limit_after(1, Some(1800), "30 min").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_to_1_h() {
    task_limit_after(2, Some(3600), "1 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_to_2_h() {
    task_limit_after(3, Some(7200), "2 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_to_4_h() {
    task_limit_after(4, Some(14_400), "4 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_to_8_h() {
    task_limit_after(5, Some(28_800), "8 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_task_limit_back_to_off() {
    task_limit_after(6, None, "Off").await;
}

async fn session_limit_after(presses: usize, secs: Option<u32>, label: &str) {
    let (status, shown) = team_settings_after(&vec!['l'; presses]).await;
    assert_eq!(
        (status.turn_limit_secs, status.session_limit_secs),
        (None, secs)
    );
    shows_once(&shown, &format!("Session  {label}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_to_30_min() {
    session_limit_after(1, Some(1800), "30 min").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_to_1_h() {
    session_limit_after(2, Some(3600), "1 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_to_2_h() {
    session_limit_after(3, Some(7200), "2 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_to_4_h() {
    session_limit_after(4, Some(14_400), "4 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_to_8_h() {
    session_limit_after(5, Some(28_800), "8 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_session_limit_back_to_off() {
    session_limit_after(6, None, "Off").await;
}

async fn idle_stop_after(presses: usize, secs: Option<u32>, label: &str) {
    let (status, shown) = team_settings_after(&vec!['i'; presses]).await;
    assert_eq!(status.idle_stop_secs, secs);
    shows_once(&shown, &format!("Idle stop  {label}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_idle_stop_to_1_h() {
    idle_stop_after(1, Some(3600), "1 h").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_idle_stop_to_off() {
    idle_stop_after(2, None, "Off").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_idle_stop_to_2_min() {
    idle_stop_after(3, Some(120), "2 min").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_set_the_idle_stop_back_to_10_min() {
    idle_stop_after(4, Some(600), "10 min").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_make_a_harness_switch_start_fresh() {
    let (status, shown) = team_settings_after(&['h']).await;
    assert_eq!(status.on_harness_switch, crate::team::SwitchMode::Fresh);
    shows_once(&shown, "On harness switch  Start fresh");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_settings_make_a_harness_switch_carry_over_again() {
    let (status, shown) = team_settings_after(&['h', 'h']).await;
    assert_eq!(status.on_harness_switch, crate::team::SwitchMode::Carry);
    shows_once(&shown, "On harness switch  Carry over");
}
/// Refreshes and pumps the shell, as the run loop does, until its top screen is the given
/// place. Takes the shell, the connection and the place. Returns nothing; fails with the
/// screen shown after 30 s.
async fn until_place(shell: &mut Shell, connection: &mut Connection, place: &Place) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while shell.place().as_ref() != Some(place) {
        assert!(
            Instant::now() < deadline,
            "never opened {place:?}:\n{}",
            render(shell, 100, 40)
        );
        shell.refresh();
        pump(shell, connection).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A session switch the owner starts from the live master view (S2, fix/session-resume-open
/// e4cb1277) opens that view again once the engine applied it: Ctrl-] then l there resumes
/// the parked session from the sessions list, Ctrl-] then n there starts a new one, and each
/// lands back on the master view on the session the engine now holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_switch_from_the_master_view_opens_the_master_view_again() {
    let mut engine = Engine::start().await;
    let (notes, mut shell, mut connection, first) =
        conversation_with_a_settled_turn(&mut engine).await;
    let master = Place::Master {
        id: notes.clone(),
        name: "Notes CLI".into(),
    };
    shell.key(Key::Esc);
    until(&mut shell, &mut connection, |shown| {
        shown.contains("Session  started")
    })
    .await;
    shell.key(Key::Char('n'));
    until(&mut shell, &mut connection, |shown| {
        shown.contains("No messages yet.")
    })
    .await;
    say(&mut shell, &mut connection, "Plan the second slice").await;
    let second = settled_master(&mut engine, &notes, &mut shell, &mut connection, 1).await;
    assert_ne!(second.session_id, first.session_id);

    say(&mut shell, &mut connection, "/master").await;
    until_place(&mut shell, &mut connection, &master).await;
    shell.key(LEADER);
    shell.key(Key::Char('l'));
    let shown = until(&mut shell, &mut connection, |shown| {
        shown.contains("Sessions  2")
    })
    .await;
    assert_ne!(shell.place().as_ref(), Some(&master), "{shown}");
    let parked = shown
        .lines()
        .find(|line| line.contains("Parked"))
        .unwrap_or_else(|| panic!("no parked row:\n{shown}"));
    assert!(parked.contains("Plan the first slice"), "{shown}");
    if !parked.contains("> Parked") {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    until_place(&mut shell, &mut connection, &master).await;
    let (resumed, _) = latest_master(&mut engine, &notes).await;
    assert_eq!(resumed.unwrap().session_id, first.session_id);

    shell.key(LEADER);
    shell.key(Key::Char('n'));
    until_place(&mut shell, &mut connection, &master).await;
    let (fresh, _) = latest_master(&mut engine, &notes).await;
    let fresh = fresh.unwrap();
    assert_ne!(fresh.session_id, first.session_id);
    assert_ne!(fresh.session_id, second.session_id);
}

fn shell_on(place: &Place) -> Shell {
    let mut ctx = ctx(100, 40);
    let screen = open(place, &mut ctx);
    Shell::with(ctx, screen)
}

async fn stays_on(shell: &mut Shell, connection: &mut Connection, place: &Place, why: &str) {
    for _ in 0..3 {
        shell.refresh();
        pump(shell, connection).await;
        assert_eq!(
            shell.place().as_ref(),
            Some(place),
            "{why}:\n{}",
            render(shell, 100, 40)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn leave_the_master_view(shell: &mut Shell, project: &ProjectId, name: &str) {
    assert_eq!(
        shell.place(),
        Some(Place::Master {
            id: project.clone(),
            name: name.into(),
        }),
        "the Conversation tab of a live master opens its terminal"
    );
    shell.key(Key::Esc);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: project.clone(),
            name: name.into(),
            tab: ProjectTab::Conversation,
        })
    );
}

/// The Conversation tab of a Codex or Claude Code master is that master's live terminal, as
/// on the desktop (N-T1 B): a page opened on the tab opens the master view once the stored
/// plan is read, Esc there lands on the tab's console and stays, and walking off the tab and
/// back opens the view again. A key typed on the tab before the plan is read keeps the
/// console, and an RCLI master keeps the console.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_conversation_tab_of_a_live_master_is_its_terminal() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let conversation = Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab: ProjectTab::Conversation,
    };
    let master = Place::Master {
        id: notes.clone(),
        name: "Notes CLI".into(),
    };
    let mut connection = engine.connect();
    let mut shell = shell_on(&conversation);
    until_place(&mut shell, &mut connection, &master).await;

    shell.key(Key::Esc);
    stays_on(
        &mut shell,
        &mut connection,
        &conversation,
        "Esc lands on the tab's console and stays there",
    )
    .await;
    shell.key(Key::ShiftRight);
    pump(&mut shell, &mut connection).await;
    let next = shell.place();
    assert!(
        next.as_ref() != Some(&conversation) && next.as_ref() != Some(&master),
        "the tab keys pass the Conversation tab: {next:?}"
    );
    shell.key(Key::ShiftLeft);
    until_place(&mut shell, &mut connection, &master).await;

    let mut early_connection = engine.connect();
    let mut early = shell_on(&conversation);
    early.key(Key::Char('h'));
    stays_on(
        &mut early,
        &mut early_connection,
        &conversation,
        "a key typed before the plan is read keeps the console",
    )
    .await;

    let rcli = TeamPlan {
        master: "rcli".into(),
        master_tools: true,
        sub_harnesses: vec![],
        min_agents: 1,
        max_agents: 1,
    };
    let local = engine
        .project_led_by("local-notes", "Local Notes", 2, Some(rcli))
        .await;
    let local_conversation = Place::Project {
        id: local,
        name: "Local Notes".into(),
        tab: ProjectTab::Conversation,
    };
    let mut local_connection = engine.connect();
    let mut local_shell = shell_on(&local_conversation);
    stays_on(
        &mut local_shell,
        &mut local_connection,
        &local_conversation,
        "an RCLI master keeps the console",
    )
    .await;
}

/// Saves Live terminal for a project as the desktop does. Inputs: the engine, the project and
/// the choice. Output: none; fails unless the engine applied it.
async fn desktop_live_terminal(engine: &Engine, project: &ProjectId, on: bool) {
    let client = Client::new(
        engine.root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut desktop) = OwnerController::attach_configured(client, OwnerClientKind::Desktop)
        .await
        .unwrap();
    let action = crate::team::TeamAction::MasterLiveTerminal {
        project_id: project.clone(),
        on,
    };
    match desktop.team_action(action, 1).await.unwrap() {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(
                operation.result,
                crate::engine::owner::OwnerResult::Applied,
                "the Live terminal choice applies"
            )
        }
        other => panic!("Live terminal not applied: {other:?}"),
    }
}

/// Live terminal Off keeps the console on every Conversation entry, with no open, no bounce
/// and no warning about the owner's own choice. A read showing On then opens the view while
/// the entry stands, and a key typed on the tab withdraws it (DEC-117 Q4, OWNER-Q4-2-L2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_entry_under_live_terminal_off_keeps_the_console_and_says_nothing() {
    const OFF: &str = "Live terminal is off for this project.";
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let conversation = Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab: ProjectTab::Conversation,
    };
    let master = Place::Master {
        id: notes.clone(),
        name: "Notes CLI".into(),
    };
    desktop_live_terminal(&engine, &notes, false).await;
    let mut connection = engine.connect();
    let mut shell = shell_on(&conversation);
    stays_on(
        &mut shell,
        &mut connection,
        &conversation,
        "Off keeps the console when the page opens",
    )
    .await;
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains(OFF), "no warning on open:\n{shown}");

    shell.key(Key::ShiftRight);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    stays_on(
        &mut shell,
        &mut connection,
        &conversation,
        "Off keeps the console when the tab is entered",
    )
    .await;
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains(OFF), "no warning on entry:\n{shown}");

    desktop_live_terminal(&engine, &notes, true).await;
    until_place(&mut shell, &mut connection, &master).await;

    shell.key(Key::Esc);
    stays_on(
        &mut shell,
        &mut connection,
        &conversation,
        "Esc lands on the console",
    )
    .await;
    desktop_live_terminal(&engine, &notes, false).await;
    for _ in 0..5 {
        stays_on(
            &mut shell,
            &mut connection,
            &conversation,
            "a read under Off, with the page already on the tab",
        )
        .await;
    }
    shell.key(Key::ShiftRight);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    stays_on(
        &mut shell,
        &mut connection,
        &conversation,
        "entering again under Off",
    )
    .await;
    shell.key(Key::Char('h'));
    desktop_live_terminal(&engine, &notes, true).await;
    for _ in 0..6 {
        stays_on(
            &mut shell,
            &mut connection,
            &conversation,
            "a key typed on the tab withdraws the entry, so On opens nothing",
        )
        .await;
    }
}

/// A read taken before Team settings opened does not decide the next Conversation entry: with
/// Live terminal turned Off there, that entry keeps the console, with no open, no bounce and
/// no warning (rv-secreview q4-3 F1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_from_before_team_settings_does_not_decide_the_next_conversation_entry() {
    const OFF: &str = "Live terminal is off for this project.";
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let tab = |tab: ProjectTab| Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab,
    };
    let settings = Place::TeamSettings {
        id: notes.clone(),
        name: "Notes CLI".into(),
    };
    let mut connection = engine.connect();
    let mut shell = shell_on(&tab(ProjectTab::Agents));
    stays_on(
        &mut shell,
        &mut connection,
        &tab(ProjectTab::Agents),
        "the first read lands with Live terminal On",
    )
    .await;

    shell.key(Key::Char('s'));
    until_place(&mut shell, &mut connection, &settings).await;
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('V'));
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("Live terminal is off: the master"),
        "V saved Off:\n{shown}"
    );

    shell.key(Key::Esc);
    shell.key(Key::ShiftLeft);
    for _ in 0..5 {
        stays_on(
            &mut shell,
            &mut connection,
            &tab(ProjectTab::Conversation),
            "the entry after Off in Team settings",
        )
        .await;
    }
    let shown = render(&mut shell, 100, 40);
    assert!(!shown.contains(OFF), "no warning on entry:\n{shown}");
}

/// N-T1 B, Switch tabs: Shift-←→ in the master view move the project page one tab, so a
/// walk across the tabs passes the Conversation tab of a live master.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shift_arrows_walk_the_project_tabs_across_the_master_view() {
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 1).await;
    let tab = |tab: ProjectTab| Place::Project {
        id: notes.clone(),
        name: "Notes CLI".into(),
        tab,
    };
    let master = Place::Master {
        id: notes.clone(),
        name: "Notes CLI".into(),
    };
    let mut connection = engine.connect();
    let mut shell = shell_on(&tab(ProjectTab::Overview));
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftRight);
    until_place(&mut shell, &mut connection, &master).await;
    shell.key(Key::ShiftRight);
    assert_eq!(shell.place(), Some(tab(ProjectTab::Agents)));
    shell.key(Key::ShiftLeft);
    until_place(&mut shell, &mut connection, &master).await;
    shell.key(Key::ShiftLeft);
    assert_eq!(shell.place(), Some(tab(ProjectTab::Overview)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_reconcile_the_full_queue_refuses_keeps_the_submission_and_reconciles_again() {
    use crate::tui::client::{CreationEvent, MutationDelivery};
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
    shell.key(Key::Enter);
    let submission = std::iter::from_fn(|| shell.ctx.next_request())
        .find_map(|request| match request {
            Request::Creation(job) => Some(job.submission),
            _ => None,
        })
        .expect("Enter sends the creation");
    let mut filled = 0;
    while filled < 64
        && shell.ctx.try_request(Request::EditPool {
            change: crate::tui::seam::PoolChange::Remove {
                id: CredentialRef::new("pool-a").unwrap(),
                opened_projects: 0,
            },
        })
    {
        filled += 1;
    }
    assert!(filled < 64, "the request queue is bounded");

    shell.arrive(owner_arrival(OwnerEvent::Creation(Box::new(
        CreationEvent {
            submission: submission.clone(),
            operation: None,
            result: Err("trusted_owner_connection_failed".into()),
            retained: true,
        },
    ))));
    assert_eq!(shell.place(), Some(Place::NewProject));
    let shown = render(&mut shell, 80, 24);
    assert!(
        !shown.contains("create the project again"),
        "a creation whose outcome is unknown is never offered as a fresh create: {shown}"
    );

    while shell.ctx.next_request().is_some() {}
    shell.tick();
    let reconciles = std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Creation(job) => Some(*job),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reconciles.len(),
        1,
        "the refused reconcile goes out once the queue has room"
    );
    assert!(matches!(
        reconciles[0].delivery,
        MutationDelivery::Reconcile
    ));
    assert_eq!(
        reconciles[0].submission, submission,
        "the same submission and project id, never a second one"
    );
}

/// Opens New project on a real engine, names the project Tidepool and presses Enter on
/// Review, then takes every request that event made. Takes the engine. Returns the shell,
/// its connection and the submission the creation sent.
async fn submitted_creation(
    engine: &Engine,
) -> (Shell, Connection, crate::tui::client::CreationSubmission) {
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
    shell.key(Key::Enter);
    let submission = std::iter::from_fn(|| shell.ctx.next_request())
        .find_map(|request| match request {
            Request::Creation(job) => Some(job.submission),
            _ => None,
        })
        .expect("Enter sends the creation");
    while shell.ctx.next_request().is_some() {}
    (shell, connection, submission)
}

/// Hands the page one creation reply from the engine worker. Takes the shell, the
/// submission, the result and whether the worker still holds the submission. Returns the
/// creation requests the page sent in answer, oldest first.
fn creation_reply(
    shell: &mut Shell,
    submission: &crate::tui::client::CreationSubmission,
    result: Result<Box<OwnerResponse>, String>,
    retained: bool,
) -> Vec<crate::tui::client::CreationJob> {
    shell.arrive(owner_arrival(OwnerEvent::Creation(Box::new(
        crate::tui::client::CreationEvent {
            submission: submission.clone(),
            operation: None,
            result,
            retained,
        },
    ))));
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Creation(job) => Some(*job),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconcile_that_finds_the_creation_unrecorded_retries_it_and_a_busy_retry_the_engine_dropped_says_busy()
 {
    use crate::tui::client::MutationDelivery;
    let engine = Engine::start().await;
    let (mut shell, _connection, submission) = submitted_creation(&engine).await;

    let sent = creation_reply(
        &mut shell,
        &submission,
        Err("trusted_owner_connection_failed".into()),
        true,
    );
    assert_eq!(sent.len(), 1, "a lost reply is reconciled once");
    assert!(matches!(sent[0].delivery, MutationDelivery::Reconcile));

    let sent = creation_reply(
        &mut shell,
        &submission,
        Ok(Box::new(OwnerResponse::NotRecordedAtBarrier {
            revision: crate::protocol::Cursor::new(0),
        })),
        true,
    );
    assert_eq!(
        sent.len(),
        1,
        "a creation the engine never recorded is sent again as the same request: {}",
        render(&mut shell, 160, 24)
    );
    assert!(matches!(sent[0].delivery, MutationDelivery::Retry));
    assert_eq!(
        sent[0].submission, submission,
        "the same submission and project id, never a second one"
    );

    let sent = creation_reply(
        &mut shell,
        &submission,
        Ok(Box::new(OwnerResponse::Rejected {
            code: crate::tui::seam::OWNER_BUSY.to_owned(),
        })),
        false,
    );
    assert!(sent.is_empty());
    let shown = render(&mut shell, 160, 24);
    assert!(
        shown.contains("CrazyAgent is busy with other windows."),
        "a busy retry the engine dropped changed nothing, so the page says busy: {shown}"
    );
    shell.tick();
    assert!(
        std::iter::from_fn(|| shell.ctx.next_request())
            .all(|request| !matches!(request, Request::Creation(_))),
        "nothing is held for the busy retry, so nothing is sent again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_lost_reply_on_a_recovering_creation_says_the_project_is_unconfirmed() {
    let engine = Engine::start().await;
    let (mut shell, _connection, submission) = submitted_creation(&engine).await;
    let lost = || Err("trusted_owner_connection_failed".to_owned());

    let sent = creation_reply(&mut shell, &submission, lost(), true);
    assert_eq!(sent.len(), 1, "a lost reply is reconciled once");
    let sent = creation_reply(&mut shell, &submission, lost(), true);
    assert!(sent.is_empty(), "recovery runs once");
    let shown = render(&mut shell, 160, 24);
    assert!(
        shown.contains("CrazyAgent could not confirm the new project."),
        "the reconcile's reply was lost too, so the outcome is still unknown: {shown}"
    );
}

/// While the uncommitted-move banner takes rows above the tabs, the Changes tab offers
/// Accept on a moved diff only once its last line was painted in the frame the owner
/// sees, never from the taller view drawn after each key (RVA-1 r1 F2).
#[test]
fn with_the_move_banner_showing_accept_waits_for_the_moved_diffs_last_line_painted() {
    use crate::engine::owner::UncommittedImport;
    use crate::landing::{
        AgentKind, ChangeAgent, ChangeDiff, ChangeSetId, ChangeSetView, ChangeState, TurnOutcome,
    };
    use crate::tui::client::changes::{ChangesEvent, ChangesQuery, ChangesView};
    use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
    let notes = ProjectId::new("notes-cli").unwrap();
    let mut ctx = ctx(100, 30);
    ctx.link = crate::tui::ctx::Link::Ready;
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Changes,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let asked = |shell: &mut Shell| {
        let (mut changes, mut process) = (Vec::new(), Vec::new());
        while let Some(request) = shell.ctx.next_request() {
            match request {
                Request::Changes(query) => changes.push(*query),
                Request::Process(query) => process.push(*query),
                _ => {}
            }
        }
        (changes, process)
    };
    let answer = |shell: &mut Shell, query: ChangesQuery, result: Result<ChangesView, String>| {
        shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Changes(Box::new(
            ChangesEvent { query, result },
        )))));
    };
    let set = |head: char| ChangeSetView {
        id: ChangeSetId::new("1".repeat(32)).unwrap(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-1".into(),
            label: crate::team::AgentRole::Sub.agent_label(1),
        },
        state: ChangeState::Pending,
        base: "b".repeat(40),
        head: head.to_string().repeat(40),
        files: 1,
        added: 60,
        removed: 0,
        last_outcome: TurnOutcome::Succeeded,
        checked: false,
        created_ms: 1,
        updated_ms: 2,
        restorable_until_ms: None,
        conflicts: Vec::new(),
        message: "Add lines".into(),
        waiting: false,
        outside: false,
        regenerated: Vec::new(),
        number: None,
    };
    let page = |text: &str, next: Option<u16>| {
        Ok(ChangesView::Diff(ChangeDiff {
            files: Vec::new(),
            text: text.into(),
            next,
            truncated: false,
        }))
    };

    let (changes, process) = asked(&mut shell);
    let uncommitted = ProcessQuery::Uncommitted {
        project: notes.clone(),
    };
    assert!(process.contains(&uncommitted), "{process:?}");
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(
        ProcessEvent {
            query: uncommitted,
            result: Ok(ProcessReply::Uncommitted(UncommittedImport {
                files: vec!["CLAUDE.md".into()],
                branch: Some("main".into()),
                checked_out: Some("main".into()),
            })),
        },
    )))));
    assert_eq!(
        changes,
        [ChangesQuery::List {
            project: notes.clone()
        }]
    );
    answer(
        &mut shell,
        changes[0].clone(),
        Ok(ChangesView::List(vec![set('1')])),
    );
    shell.key(Key::Enter);
    let (changes, _) = asked(&mut shell);
    assert!(
        matches!(changes[..], [ChangesQuery::Diff { page: 0, .. }]),
        "{changes:?}"
    );
    answer(&mut shell, changes[0].clone(), page("+one\n", Some(1)));
    let (changes, _) = asked(&mut shell);
    assert!(
        matches!(changes[..], [ChangesQuery::Diff { page: 1, .. }]),
        "{changes:?}"
    );
    answer(&mut shell, changes[0].clone(), Err("changes_stale".into()));
    let (changes, _) = asked(&mut shell);
    assert_eq!(
        changes,
        [ChangesQuery::List {
            project: notes.clone()
        }]
    );
    answer(
        &mut shell,
        changes[0].clone(),
        Ok(ChangesView::List(vec![set('2')])),
    );
    let (changes, _) = asked(&mut shell);
    assert!(
        matches!(&changes[..], [ChangesQuery::Diff { page: 0, head, .. }] if *head == "2".repeat(40)),
        "{changes:?}"
    );
    let long: String = (1..=60).map(|line| format!("+line {line}\n")).collect();
    answer(&mut shell, changes[0].clone(), page(&long, None));

    let first = render(&mut shell, 100, 30);
    assert!(
        first.contains("Git still shows CLAUDE.md as deleted"),
        "the banner shows\n{first}"
    );
    assert!(first.contains("+line 1"), "{first}");
    let offered = |shown: &str| shown.lines().last().unwrap_or("").contains("a Accept");
    let mut painted_end = false;
    for _ in 0..80 {
        let shown = render(&mut shell, 100, 30);
        painted_end = shown.lines().any(|row| row.trim() == "+line 60");
        assert!(
            painted_end || !offered(&shown),
            "Accept is offered while the moved diff's last line was never painted\n{shown}"
        );
        if painted_end {
            break;
        }
        shell.key(Key::Down);
    }
    assert!(painted_end, "scrolling reaches the last line");
    shell.key(Key::Down);
    let shown = render(&mut shell, 100, 30);
    assert!(
        offered(&shown),
        "once the last line was painted, Accept comes back\n{shown}"
    );
}
