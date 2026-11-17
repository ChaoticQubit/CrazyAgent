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
use crate::github::{
    GitHubIgnoreDraft, GitHubMethod, GitHubOwner, GitHubOwners, GitHubPublishJob,
    GitHubPublishPlan, GitHubPublishStep, GitHubRuleOutcome, GitHubStatus,
};
use crate::master_session::{
    MasterOwnerAction, MasterSessionPhase, MasterSessionView, MasterTurnView,
};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind, SetupQuery, SetupView, TeamPlan};
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobKind, JobState, JobSubject, JobView};
use crate::protocol::{ConfigId, CredentialRef, ProjectId, ProjectSpec, RequestId, SessionId};
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView};
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
    ctx.store.learning = Load::NotReady;
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
                        SetupQuery::Overview { .. } => "overview",
                        SetupQuery::RefreshOverview { .. } => "overview refresh",
                    });
                    connection.project_setup(query).unwrap();
                }
                Request::Intake(job) => {
                    sent.push(match job.query() {
                        IntakeQuery::JobStart { .. } => "job start",
                        IntakeQuery::JobStatus => "job status",
                        IntakeQuery::JobDismiss { .. } => "job dismiss",
                        IntakeQuery::Found => "found",
                        IntakeQuery::GitHubPublishStart { .. } => "github publish",
                        IntakeQuery::GitHubPublishStatus { .. } => "github publish status",
                        IntakeQuery::GitHubPublishPrompt { .. } => "github publish prompt",
                        _ => "intake",
                    });
                    connection.intake(*job).unwrap();
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
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://LOCAL_ADDRESS/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
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
    _refusing: [std::net::TcpStream; 2],
    #[cfg(target_os = "macos")]
    _keychain: crate::credentials::test_keychain::TestKeychain,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-page-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        #[cfg(target_os = "macos")]
        let keychain = {
            let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
            keychain.serve(&crate::github::namespace(root.path()));
            keychain
        };
        let (refusing, settings) = crate::tui::test_home::refusing_settings(SETTINGS);
        let global = root.path().join("global.toml");
        std::fs::write(&global, &settings).unwrap();
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
            _refusing: refusing,
            #[cfg(target_os = "macos")]
            _keychain: keychain,
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

    /// Stops this engine, its owner client first, and starts a new one on the same data folder,
    /// running `between` on that folder while none runs, as quitting and opening the app does.
    /// Input: what to do in between. Output: the engine started again, with a new owner client.
    #[cfg(target_os = "macos")]
    async fn restart(self, between: impl FnOnce(&std::path::Path)) -> Self {
        let Self {
            root,
            home,
            _service: mut service,
            _stop: stop,
            controller,
            _refusing: refusing,
            _keychain: keychain,
        } = self;
        drop(controller);
        service.shutdown().await.unwrap();
        drop(stop);
        between(root.path());
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &root.path().join("global.toml"),
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
        Self {
            root,
            home,
            _service: service,
            _stop: stop,
            controller,
            _refusing: refusing,
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
        sent.iter()
            .filter(|label| **label == "github publish status")
            .count(),
        1,
        "the Overview reads the project's GitHub publish once: {sent:?}"
    );
    assert_eq!(
        sent.iter()
            .filter(|label| **label == "github publish prompt")
            .count(),
        1,
        "a project with no publish asks once whether to offer one: {sent:?}"
    );
    let sent: Vec<&str> = sent
        .into_iter()
        .filter(|label| !matches!(*label, "github publish status" | "github publish prompt"))
        .collect();
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
            "overview",
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

/// DEC-177: m then m on Settings starts the engine's job that moves a project kept outside
/// the projects folder into it, says so and lets the owner keep working. When the job ends
/// its end is said, the project list and the project's setup are read again, and the folder
/// is in the projects folder and gone from its old place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_kept_outside_the_projects_folder_moves_in_as_the_engine_s_job() {
    let mut engine = Engine::start().await;
    let notes = ProjectId::new("notes-cli").unwrap();
    let made = engine
        .controller
        .setup_project(
            notes.clone(),
            ProjectSpec {
                name: "Notes CLI".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Notes CLI keeps notes.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Cli],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            made,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ),
        "{made:?}"
    );
    let origin = std::fs::canonicalize(engine.home.path())
        .unwrap()
        .join("notes-cli");
    assert!(origin.is_dir(), "{origin:?}");
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 2).await;
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .join("notes-cli");
    let mut connection = engine.connect();
    let mut ctx = ctx(120, 30);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::ShiftLeft);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('M'));
    let shown = render(&mut shell, 120, 30);
    assert!(
        shown.contains("Move Notes CLI into your CrazyAgent folder?"),
        "{shown}"
    );
    shell.key(Key::Char('m'));
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"job start"), "{sent:?}");
    let said = shell.ctx.message().expect("the start is said");
    assert_eq!(
        (said.lead.as_str(), said.rest.as_str()),
        (
            "Moving Notes CLI.",
            "Follow it on Home; you can keep working."
        )
    );
    assert!(shell.ctx.store.intake.is_some(), "the started job is kept");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = Vec::new();
    let ended = loop {
        let ended = shell.ctx.store.intake.clone();
        if let Some(job) = ended.filter(|job| !super::intake_job::running(job)) {
            break job;
        }
        assert!(
            Instant::now() < deadline,
            "the move never ended: {:?}",
            shell.ctx.store.intake
        );
        super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
        last = pump(&mut shell, &mut connection).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(ended.state, JobState::Done, "{ended:?}");
    assert_eq!(ended.kind, JobKind::MoveProject);
    assert_eq!(ended.project_id.as_ref(), Some(&notes));
    assert!(last.contains(&"projects"), "{last:?}");
    assert!(last.contains(&"setup"), "{last:?}");
    let said = shell.ctx.message().expect("the end is said");
    assert_eq!(
        said.lead,
        "notes-cli is now in your projects folder. Start the team again from Team."
    );
    assert!(target.is_dir(), "{target:?}");
    assert!(!origin.exists(), "{origin:?}");
}

/// Makes one project with no research in the engine's first projects folder, as setup does.
/// Inputs: the engine, the project id and name, and the setup's generation. Output: the project
/// and its folder.
async fn made_in_first_home(
    engine: &mut Engine,
    id: &str,
    name: &str,
    generation: u64,
) -> (ProjectId, std::path::PathBuf) {
    let project = ProjectId::new(id).unwrap();
    let made = engine
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
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            generation,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            made,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ),
        "{made:?}"
    );
    let folder = std::fs::canonicalize(engine.home.path()).unwrap().join(id);
    assert!(folder.is_dir(), "{folder:?}");
    (project, folder)
}

/// Runs statements on the test engine's database, for a stand-in no client can make, such as an
/// agent slow to stop. Inputs: the engine and the statements. Output: none.
fn engine_sql(engine: &Engine, statements: &str) {
    let database = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(10)).unwrap();
    database.execute_batch(statements).unwrap();
}

/// Reads the engine's job on the shell until it holds one the check accepts. Inputs: the shell,
/// its connection and the check. Output: the job and the requests of the last pump.
async fn job_until(
    shell: &mut Shell,
    connection: &mut Connection,
    check: impl Fn(&JobView) -> bool,
) -> (JobView, Vec<&'static str>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = Vec::new();
    loop {
        if let Some(job) = shell.ctx.store.intake.clone().filter(&check) {
            return (job, last);
        }
        assert!(
            Instant::now() < deadline,
            "no such job: {:?}",
            shell.ctx.store.intake
        );
        super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
        last = pump(shell, connection).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Opens a project's page on Settings and asks its move twice, as M opens the question and m
/// moves. Inputs: the shell, its connection, the project and its name. Output: the requests sent
/// after the second m.
async fn move_from_settings(
    shell: &mut Shell,
    connection: &mut Connection,
    name: &str,
) -> Vec<&'static str> {
    pump(shell, connection).await;
    shell.key(Key::ShiftLeft);
    pump(shell, connection).await;
    shell.key(Key::Char('M'));
    let shown = render(shell, 120, 30);
    assert!(
        shown.contains(&format!("Move {name} into your CrazyAgent folder?")),
        "{shown}"
    );
    shell.key(Key::Char('m'));
    pump(shell, connection).await
}

/// A fresh terminal on a project's page. Inputs: the project and its name. Output: the shell.
fn project_shell(project: &ProjectId, name: &str) -> Shell {
    let mut ctx = ctx(120, 30);
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: name.into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    Shell::with(ctx, screen)
}

/// A fresh terminal on Home that has read the engine's job, as starting the TUI again does.
/// The owner opens and clears the filter before the reads land, so Home keeps its focus on
/// the list: this engine's one account sits at a port that refuses connections, so setup is
/// never done and Home would otherwise move the focus to Get set up, where Enter opens
/// Accounts. Inputs: the connection. Output: the shell.
async fn home_shell(connection: &mut Connection) -> Shell {
    let mut ctx = ctx(120, 30);
    let screen = open(&Place::Home, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    shell.key(Key::Char('/'));
    shell.key(Key::Esc);
    pump(&mut shell, connection).await;
    super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
    pump(&mut shell, connection).await;
    shell
}

/// Puts the focus of a Home that a page went to on its list, where Enter acts on the job
/// card. Once its checks land, Home moves the focus to Get set up, since setup is never done
/// here (see `home_shell`), and its key bar then starts with that panel's Enter; Tab moves it
/// back. No reply arrives before the next key, so the focus holds. Input: the shell on Home.
#[cfg(target_os = "macos")]
fn list_focus(shell: &mut Shell) {
    let setup_keys = |shell: &mut Shell| {
        render(shell, 120, 30)
            .lines()
            .last()
            .is_some_and(|keys| keys.trim_start().starts_with("Enter Add an account"))
    };
    if setup_keys(shell) {
        shell.key(Key::Tab);
    }
    assert!(!setup_keys(shell), "Home's focus is on its list");
}

/// DEC-177: a project move whose name is taken in the projects folder ends failed with the
/// reason, the project and the folder already there untouched; a terminal started again shows
/// the ended move on Home and d forgets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_move_whose_name_is_taken_fails_with_the_reason_and_d_on_home_forgets_it() {
    let mut engine = Engine::start().await;
    let (notes, origin) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 2).await;
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .join("notes-cli");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("other.txt"), b"another folder's file\n").unwrap();
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    let sent = move_from_settings(&mut shell, &mut connection, "Notes CLI").await;
    assert_eq!(sent.first(), Some(&"job start"), "{sent:?}");
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert!(
        matches!(&ended.state, JobState::Failed { code, .. } if code == "home_destination_exists"),
        "{ended:?}"
    );
    let said = shell.ctx.message().expect("the end is said");
    assert_eq!(
        said.lead,
        "notes-cli: Your projects folder already has a folder with this name. Rename one of them first."
    );
    assert!(origin.is_dir(), "{origin:?}");
    assert_eq!(
        std::fs::read(target.join("other.txt")).unwrap(),
        b"another folder's file\n"
    );
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    assert_eq!(
        shell.ctx.store.intake.as_ref().map(|job| &job.id),
        Some(&ended.id),
        "a terminal started again shows the ended move"
    );
    shell.key(Key::Home);
    shell.key(Key::Char('d'));
    pump(&mut shell, &mut connection).await;
    pump(&mut shell, &mut connection).await;
    assert_eq!(shell.ctx.store.intake, None, "d forgets the ended move");
}

/// DEC-177: a project move waits for the project's agents to stop, says how many are left in
/// the header on every screen with nothing moved, and moves once the last one stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_move_waits_for_its_agents_says_how_many_are_left_and_moves_once_they_stopped() {
    let mut engine = Engine::start().await;
    let (notes, origin) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    std::fs::write(origin.join("notes.txt"), b"kept\n").unwrap();
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 2).await;
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .join("notes-cli");
    let copy = engine.root.path().join("agent-43");
    engine_sql(
        &engine,
        &format!(
            "INSERT OR IGNORE INTO team_paused_projects(project,since_ms) VALUES ('notes-cli',1);
             INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms)
             VALUES (43,'notes-cli','sub','codex',1,'{}','working',0,0);",
            copy.to_str().unwrap()
        ),
    );
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    let sent = move_from_settings(&mut shell, &mut connection, "Notes CLI").await;
    assert_eq!(sent.first(), Some(&"job start"), "{sent:?}");
    let (waiting, _) = job_until(&mut shell, &mut connection, |job| {
        matches!(job.state, JobState::Stopping { .. })
    })
    .await;
    assert_eq!(waiting.state, JobState::Stopping { agents_left: 1 });
    let shown = render(&mut shell, 120, 30);
    assert!(shown.contains("Stopping notes-cli: 1 agent"), "{shown}");
    assert!(
        origin.join("notes.txt").is_file(),
        "nothing moves while it waits"
    );
    assert!(!target.exists());
    engine_sql(
        &engine,
        "UPDATE team_agents SET status='stopped' WHERE id=43;",
    );
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert_eq!(ended.state, JobState::Done, "{ended:?}");
    assert!(!origin.exists());
    assert_eq!(std::fs::read(target.join("notes.txt")).unwrap(), b"kept\n");
}

/// DEC-177: one job at a time. While one project's copy runs, another project's move is
/// refused naming the running move; a start for the running project finds its job; a terminal
/// started again shows the running move on Home, where Enter stops it with nothing moved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_project_s_move_is_refused_naming_the_running_move_and_a_new_terminal_shows_it() {
    let mut engine = Engine::start().await;
    let (notes, origin) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let (reading, kept) = made_in_first_home(&mut engine, "reading-list", "Reading List", 2).await;
    for part in 0..200 {
        let folder = origin.join(format!("p{part}"));
        std::fs::create_dir(&folder).unwrap();
        for file in 0..200 {
            std::fs::write(
                folder.join(format!("f{file}.txt")),
                format!("{part} {file}\n"),
            )
            .unwrap();
        }
    }
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 3).await;
    let home = std::fs::canonicalize(next.path()).unwrap();
    let copying = JobSubject::MoveProject {
        project_id: notes.clone(),
    };
    let running = engine
        .controller
        .job_start(
            RequestId::new("move-busy-1").unwrap(),
            copying.clone(),
            AddMode::Copy,
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(super::intake_job::running(&running), "{running:?}");
    let found = engine
        .controller
        .job_start(
            RequestId::new("move-busy-2").unwrap(),
            copying,
            AddMode::Copy,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        found.id, running.id,
        "a start for the running project finds its job"
    );

    let mut connection = engine.connect();
    let mut shell = project_shell(&reading, "Reading List");
    pump(&mut shell, &mut connection).await;
    super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
    let sent = move_from_settings(&mut shell, &mut connection, "Reading List").await;
    assert!(sent.contains(&"job start"), "{sent:?}");
    let said = shell.ctx.message().expect("the refusal is said");
    assert_eq!(
        said.lead,
        "Moving notes-cli is running. Wait for it to finish on Home."
    );
    assert!(kept.is_dir(), "{kept:?}");
    assert!(!home.join("reading-list").exists());
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    let shown = shell.ctx.store.intake.clone().expect("the running move");
    assert_eq!(
        shown.id, running.id,
        "a terminal started again shows the running move"
    );
    let rows = render(&mut shell, 120, 30);
    assert!(rows.contains("notes-cli"), "{rows}");
    shell.key(Key::Home);
    shell.key(Key::Enter);
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert!(
        matches!(ended.state, JobState::Cancelled { .. }),
        "{ended:?}"
    );
    assert!(origin.join("p199").join("f199.txt").is_file());
    assert!(!home.join("notes-cli").exists(), "nothing was copied");
}

/// DEC-177: a project move with a linked worktree outside it waits for the owner's answer;
/// Enter on Home's job opens the question with Keep it here first and no Use where it is, and
/// Move anyway moves that project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_move_s_soft_refusal_is_answered_from_home_and_move_anyway_moves_it() {
    let mut engine = Engine::start().await;
    let (notes, origin) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let outside = tempfile::tempdir().unwrap();
    let side = std::fs::canonicalize(outside.path())
        .unwrap()
        .join("notes-side");
    super::intake_tests::repo_with_worktree(&origin, &side);
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 2).await;
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .join("notes-cli");
    let subject = JobSubject::MoveProject {
        project_id: notes.clone(),
    };
    engine
        .controller
        .job_start(
            RequestId::new("move-ask-1").unwrap(),
            subject,
            AddMode::Move,
            Vec::new(),
        )
        .await
        .unwrap();

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    let (waiting, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert_eq!(
        waiting.state,
        JobState::NeedsAnswer {
            code: "home_move_worktrees".into()
        }
    );
    shell.key(Key::Home);
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::AnswerAdd {
            job: waiting.id.clone()
        })
    );
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("Move notes-cli anyway?"), "{shown}");
    assert!(shown.contains("Keep it here"), "{shown}");
    assert!(!shown.contains("Use where it is"), "{shown}");
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
    let said = shell.ctx.message().expect("the start is said");
    assert_eq!(
        (said.lead.as_str(), said.rest.as_str()),
        (
            "Moving notes-cli.",
            "Follow it on Home; you can keep working."
        )
    );
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        job.id != waiting.id && !super::intake_job::running(job)
    })
    .await;
    assert_eq!(ended.state, JobState::Done, "{ended:?}");
    assert_eq!(ended.project_id.as_ref(), Some(&notes));
    assert!(target.is_dir(), "{target:?}");
    assert!(!origin.exists(), "{origin:?}");
}

/// Counts what a query of the test engine's database selects, for a row to check what no client
/// reads. Inputs: the engine and the query. Output: the count.
#[cfg(target_os = "macos")]
fn engine_count(engine: &Engine, query: &str) -> i64 {
    let database = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(10)).unwrap();
    database.query_row(query, [], |row| row.get(0)).unwrap()
}

/// Adds a stand-in team agent of a project that reads working until the row flips it, as an
/// agent slow to stop. Inputs: the engine, the project and the agent's number. Output: none.
#[cfg(target_os = "macos")]
fn working(engine: &Engine, project: &str, agent: u32) {
    let worktree = engine.root.path().join(format!("agent-{agent}"));
    engine_sql(
        engine,
        &format!(
            "INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms)
             VALUES ({agent},'{project}','sub','codex',1,'{}','working',0,0);",
            worktree.to_str().unwrap()
        ),
    );
}

/// Opens a project's page on Settings, opens its remove ask, lets the engine answer what
/// removing would do, then picks Delete everything and types the exact name, so Enter deletes.
/// Inputs: the shell, its connection and the project's name. Output: none.
#[cfg(target_os = "macos")]
async fn delete_typed(shell: &mut Shell, connection: &mut Connection, name: &str) {
    pump(shell, connection).await;
    shell.key(Key::ShiftLeft);
    pump(shell, connection).await;
    shell.key(Key::Char('x'));
    let sent = pump(shell, connection).await;
    assert!(sent.contains(&"removal"), "{sent:?}");
    shell.key(Key::Down);
    for typed in name.chars() {
        shell.key(Key::Char(typed));
    }
}

/// Presses Enter on a typed Delete everything and pumps until the page went Home, as it does
/// once the engine's job started. Inputs: the shell and its connection. Output: every request
/// the pumps sent.
#[cfg(target_os = "macos")]
async fn delete_entered(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    shell.key(Key::Enter);
    let mut sent = pump(shell, connection).await;
    let deadline = Instant::now() + Duration::from_secs(8);
    while shell.place() != Some(Place::Home) {
        assert!(
            Instant::now() < deadline,
            "the delete never went Home: {:?}",
            shell.place()
        );
        sent.extend(pump(shell, connection).await);
    }
    sent
}

/// DEC-177 add.1: Delete everything on the project page starts the engine's delete job and goes
/// Home, where it waits for the project's agents with nothing moved; another project's delete
/// is refused naming it, a terminal started again shows it, and Enter there stops it, keeping
/// the project and its folder. Runs on macOS only, the one platform that offers Delete
/// everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_waits_for_its_agents_on_home_another_is_refused_and_enter_there_stops_it() {
    let mut engine = Engine::start().await;
    let (notes, folder) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let (reading, kept) = made_in_first_home(&mut engine, "reading-list", "Reading List", 2).await;
    std::fs::write(folder.join("notes.txt"), b"kept\n").unwrap();
    working(&engine, "notes-cli", 44);
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    delete_typed(&mut shell, &mut connection, "Notes CLI").await;
    let sent = delete_entered(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
    assert!(!sent.contains(&"change"), "no direct delete: {sent:?}");
    let said = shell.ctx.message().expect("the start is said");
    assert_eq!(said.lead, "Deleting Notes CLI.");
    let (waiting, _) = job_until(&mut shell, &mut connection, |job| {
        matches!(job.state, JobState::Stopping { .. })
    })
    .await;
    assert_eq!(waiting.kind, JobKind::Delete);
    assert_eq!(waiting.source, "Notes CLI");
    assert_eq!(waiting.state, JobState::Stopping { agents_left: 1 });
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains("Waiting for 1 agent of Notes CLI to stop..."),
        "{rows}"
    );
    assert!(
        folder.join("notes.txt").is_file(),
        "nothing moves while it waits"
    );
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = project_shell(&reading, "Reading List");
    pump(&mut shell, &mut connection).await;
    super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
    delete_typed(&mut shell, &mut connection, "Reading List").await;
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
    let said = shell.ctx.message().expect("the refusal is said");
    assert_eq!(
        said.lead,
        "Deleting Notes CLI is running. Wait for it to finish on Home."
    );
    assert!(kept.is_dir(), "{kept:?}");
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    let shown = shell.ctx.store.intake.clone().expect("the running delete");
    assert_eq!(
        shown.id, waiting.id,
        "a terminal started again shows the running delete"
    );
    shell.key(Key::Home);
    shell.key(Key::Enter);
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert_eq!(
        ended.state,
        JobState::Cancelled {
            phase: crate::projects_home::job::Phase::Stopping
        }
    );
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains("Deleting Notes CLI stopped. Nothing was moved to the Trash."),
        "{rows}"
    );
    assert_eq!(
        std::fs::read(folder.join("notes.txt")).unwrap(),
        b"kept\n",
        "a stopped delete keeps the folder"
    );
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM projects WHERE id='notes-cli'"
        ),
        1,
        "and the project"
    );
    engine_sql(
        &engine,
        "UPDATE team_agents SET status='stopped' WHERE id=44;",
    );
}

/// DEC-177 add.1: a delete whose agents do not stop within the wait ends failed on Home with
/// the reason, the project and its folder kept. Runs on macOS only, the one platform that
/// offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_whose_agents_never_stop_fails_on_home_with_the_reason() {
    let mut engine = Engine::start().await;
    let (notes, folder) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    working(&engine, "notes-cli", 45);
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    delete_typed(&mut shell, &mut connection, "Notes CLI").await;
    delete_entered(&mut shell, &mut connection).await;
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert!(
        matches!(&ended.state, JobState::Failed { code, .. } if code == "delete_agents_running"),
        "{ended:?}"
    );
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains("Notes CLI: Its agents did not stop in time, so nothing was deleted."),
        "{rows}"
    );
    assert!(folder.join(".crazyagent").is_dir(), "{folder:?}");
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM projects WHERE id='notes-cli'"
        ),
        1
    );
    engine_sql(
        &engine,
        "UPDATE team_agents SET status='stopped' WHERE id=45;",
    );
}

/// Gives a projects folder made read-only back its owner write bit when it is dropped, also
/// when a row panics first, so the temp folder's own drop can still remove what it holds.
/// Holds the folder.
#[cfg(target_os = "macos")]
struct Writable(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Writable {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Counts the entries of the owner's own Trash, never listing them. The folder comes from the
/// account's home in the user database, since a Trash move from the system volume goes there
/// whatever HOME says. Takes nothing. Returns the count; a Trash that cannot be read stops the
/// row with what it needs.
#[cfg(target_os = "macos")]
fn owner_trash_count() -> usize {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .expect("the account has a user record");
    let trash = user.dir.join(".Trash");
    match std::fs::read_dir(&trash) {
        Ok(entries) => entries.count(),
        Err(error) => panic!(
            "PREREQUISITE: this row counts the entries of {trash:?} to prove it moves nothing there, and reading it failed: {error}; give the terminal Full Disk Access and rerun"
        ),
    }
}

/// DEC-177 add.1: a delete whose folder cannot be moved to the Trash ends failed on Home with
/// the reason and the project kept; Enter there is Try again, which opens the project's remove
/// ask and sends no delete. The projects folder is made read-only so nothing reaches the
/// owner's Trash, its write bit comes back even when the row panics, and the owner's Trash
/// holds as many entries after the row as before. Runs on macOS only, the one platform that
/// offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_whose_trash_move_fails_says_why_on_home_and_enter_reopens_the_remove_ask() {
    let trash_before = owner_trash_count();
    let mut engine = Engine::start().await;
    let (notes, folder) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let home = std::fs::canonicalize(engine.home.path()).unwrap();
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    delete_typed(&mut shell, &mut connection, "Notes CLI").await;
    let writable = Writable(home.clone());
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    delete_entered(&mut shell, &mut connection).await;
    let (ended, _) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    drop(writable);
    assert_eq!(
        ended.state,
        JobState::Failed {
            code: "home_trash_failed".into(),
            phase: crate::projects_home::job::Phase::Trashing
        }
    );
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains(
            "Notes CLI: The folder could not be moved to the Trash. Nothing was deleted."
        ),
        "{rows}"
    );
    assert!(folder.join(".crazyagent").is_dir(), "{folder:?}");
    list_focus(&mut shell);
    shell.key(Key::Home);
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        }),
        "Try again opens the remove ask"
    );
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"removal"), "{sent:?}");
    assert!(
        !sent.contains(&"job start"),
        "nothing deletes in one click: {sent:?}"
    );
    assert!(folder.join(".crazyagent").is_dir(), "{folder:?}");
    assert_eq!(
        owner_trash_count(),
        trash_before,
        "the failed Trash move put nothing in the owner's own Trash"
    );
}

/// DEC-177 add.1: a delete ends done on Home with the project's name and its records gone, and
/// the terminal reads the projects again. Its folder is gone before Enter, so the delete moves
/// nothing to the owner's Trash. Runs on macOS only, the one platform that offers Delete
/// everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_ends_done_on_home_and_the_projects_are_read_again() {
    let mut engine = Engine::start().await;
    let (notes, folder) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let mut connection = engine.connect();
    let mut shell = project_shell(&notes, "Notes CLI");
    delete_typed(&mut shell, &mut connection, "Notes CLI").await;
    std::fs::remove_dir_all(&folder).unwrap();
    delete_entered(&mut shell, &mut connection).await;
    let (ended, last) = job_until(&mut shell, &mut connection, |job| {
        !super::intake_job::running(job)
    })
    .await;
    assert_eq!(ended.state, JobState::Done, "{ended:?}");
    let mut sent = last;
    sent.extend(pump(&mut shell, &mut connection).await);
    assert!(
        sent.contains(&"projects"),
        "the projects are read again: {sent:?}"
    );
    let said = shell.ctx.message().expect("the end is said");
    assert_eq!(
        said.lead,
        "Notes CLI is deleted. Its files are in the Trash."
    );
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM projects WHERE id='notes-cli'"
        ),
        0,
        "the records went"
    );
}

/// DEC-177 answer 3, add.1: a delete an engine stop cut short shows on Home after the next start
/// with how many of its folders reached the Trash, and never goes on by itself; Enter is Finish
/// delete, which opens the project's remove ask and sends no delete, and d keeps the project,
/// ending the banner's mark. Nothing reaches the owner's Trash. Runs on macOS only, the one
/// platform that offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_cut_short_shows_on_home_after_a_restart_and_d_keeps_the_project() {
    let mut engine = Engine::start().await;
    let (notes, folder) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let engine = engine
        .restart(|root| {
            drop(crate::ledger::open(root).unwrap());
            rusqlite::Connection::open(root.join("engine.db"))
                .unwrap()
                .execute_batch(&format!(
                    "INSERT INTO projects_delete_pending(exchange) VALUES (904);
                     INSERT INTO ledger_actions(kind,action_key,payload,started_ms)
                     VALUES ('project_delete','notes-cli',
                             '{{\"project\":\"notes-cli\",\"exchange\":904,\"request\":\"delete-cut-tui\",\"name\":\"Notes CLI\",\"total\":2}}',{started});"
                ))
                .unwrap();
        })
        .await;
    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    let cut = shell
        .ctx
        .store
        .intake
        .clone()
        .expect("the delete cut short");
    assert_eq!(cut.kind, JobKind::Delete);
    assert_eq!(
        cut.state,
        JobState::CutShort {
            trashed: 1,
            total: 2
        }
    );
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains("Deleting Notes CLI was cut short with 1 of 2 folders in the Trash."),
        "{rows}"
    );
    assert!(
        folder.join(".crazyagent").is_dir(),
        "the start never finishes it"
    );
    shell.key(Key::Home);
    shell.key(Key::Enter);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        }),
        "Finish delete opens the remove ask"
    );
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"removal"), "{sent:?}");
    assert!(
        !sent.contains(&"job start"),
        "nothing deletes in one click: {sent:?}"
    );
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    assert_eq!(
        shell.ctx.store.intake.as_ref().map(|job| &job.id),
        Some(&cut.id)
    );
    shell.key(Key::Home);
    shell.key(Key::Char('d'));
    pump(&mut shell, &mut connection).await;
    pump(&mut shell, &mut connection).await;
    assert_eq!(
        shell.ctx.store.intake, None,
        "d keeps the project and forgets the job"
    );
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM ledger_actions WHERE kind='project_delete' AND action_key='notes-cli' AND outcome IS NULL"
        ),
        0,
        "Keep the project ends the banner's mark"
    );
    assert!(folder.join(".crazyagent").is_dir(), "{folder:?}");
}

/// DEC-197: while another project's move waits for its agents, Create on New project starts
/// the engine's New Project job, which waits its turn behind the move and names it while the
/// wizard follows it; once the move is stopped the New Project is made, and the move moved
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_while_a_move_holds_the_job_waits_its_turn_and_is_made_once_the_move_stops() {
    let mut engine = Engine::start().await;
    let (notes, origin) = made_in_first_home(&mut engine, "notes-cli", "Notes CLI", 1).await;
    let next = crate::tui::test_home::projects_home(&mut engine.controller, 2).await;
    let home = std::fs::canonicalize(next.path()).unwrap();
    let copy = engine.root.path().join("agent-47");
    engine_sql(
        &engine,
        &format!(
            "INSERT OR IGNORE INTO team_paused_projects(project,since_ms) VALUES ('notes-cli',1);
             INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms)
             VALUES (47,'notes-cli','sub','codex',1,'{}','working',0,0);",
            copy.to_str().unwrap()
        ),
    );
    let held = engine
        .controller
        .job_start(
            RequestId::new("move-held-1").unwrap(),
            JobSubject::MoveProject {
                project_id: notes.clone(),
            },
            AddMode::Move,
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(super::intake_job::running(&held), "{held:?}");

    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(
        sent.iter().filter(|sent| **sent == "job start").count(),
        1,
        "one start: {sent:?}"
    );
    let moving = origin
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("the moved folder's name")
        .to_owned();
    let (waiting, _) = job_until(&mut shell, &mut connection, |job| {
        job.kind == JobKind::Setup
    })
    .await;
    assert_eq!(
        waiting.state,
        JobState::Waiting { behind: moving },
        "{waiting:?}"
    );
    assert_eq!(shell.place(), Some(Place::NewProject));
    let shown = render(&mut shell, 80, 24);
    assert!(!shown.contains("Could not create the project."), "{shown}");
    assert!(
        !home.join("tidepool").exists(),
        "nothing is made while it waits"
    );

    assert!(engine.controller.job_stop(held.id.clone()).await.unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    while !home.join("tidepool").is_dir() {
        assert!(
            Instant::now() < deadline,
            "the New Project was never made: {:?}",
            shell.ctx.store.intake
        );
        super::intake::ask(&mut shell.ctx, IntakeQuery::JobStatus);
        pump(&mut shell, &mut connection).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        origin.is_dir(),
        "the stopped move moved nothing: {origin:?}"
    );
}

/// DEC-177 step 3c: a New Project an engine stop cut short shows on Home after the next start
/// as stopped part way and never goes on by itself; Enter opens New project and starts
/// nothing, and d forgets it. Runs on macOS, where the restart helper runs.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_project_cut_short_shows_on_home_after_a_restart_and_d_forgets_it() {
    let engine = Engine::start().await;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let engine = engine
        .restart(|root| {
            drop(crate::ledger::open(root).unwrap());
            rusqlite::Connection::open(root.join("engine.db"))
                .unwrap()
                .execute_batch(&format!(
                    "INSERT INTO ledger_actions(kind,action_key,payload,started_ms)
                     VALUES ('project_setup','project-cut-tui',
                             '{{\"project\":\"project-cut-tui\",\"request\":\"setup-cut-tui\",\"name\":\"Cut Short\"}}',{started});"
                ))
                .unwrap();
        })
        .await;
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM ledger_actions WHERE kind='project_setup' AND outcome IS NULL"
        ),
        0,
        "the start ends the mark"
    );
    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    let cut = shell
        .ctx
        .store
        .intake
        .clone()
        .expect("the New Project cut short");
    assert_eq!(cut.kind, JobKind::Setup);
    assert_eq!(cut.source, "Cut Short");
    assert_eq!(
        cut.state,
        JobState::Failed {
            code: "home_new_project_interrupted".into(),
            phase: crate::projects_home::job::Phase::Building,
        }
    );
    let rows = render(&mut shell, 120, 30);
    assert!(
        rows.contains("Cut Short: Setting up the project stopped part way."),
        "{rows}"
    );
    shell.key(Key::Home);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::NewProject));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(
        !sent.contains(&"job start"),
        "nothing starts in one key: {sent:?}"
    );
    drop(shell);
    drop(connection);

    let mut connection = engine.connect();
    let mut shell = home_shell(&mut connection).await;
    assert_eq!(
        shell.ctx.store.intake.as_ref().map(|job| &job.id),
        Some(&cut.id)
    );
    shell.key(Key::Home);
    shell.key(Key::Char('d'));
    pump(&mut shell, &mut connection).await;
    pump(&mut shell, &mut connection).await;
    assert_eq!(shell.ctx.store.intake, None, "d forgets the job");
    assert_eq!(
        engine_count(
            &engine,
            "SELECT count(*) FROM projects WHERE id='project-cut-tui'"
        ),
        0,
        "a cut-short New Project leaves no project"
    );
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
            kept: None,
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

/// L388: the project page test engine seeds its local account where no model server the
/// host runs can answer, so no master or research step here reaches the owner's Ollama.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_project_page_test_engine_seeds_its_local_account_where_no_host_model_server_answers() {
    let engine = Engine::start().await;
    crate::tui::test_home::assert_no_host_model_server(&engine.root.path().join("global.toml"))
        .await;
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
    let github = sent.iter().filter(|label| **label == "intake").count();
    assert_eq!(
        github, 1,
        "the wizard reads the GitHub sign-in once: {sent:?}"
    );
    let sent: Vec<&str> = sent
        .into_iter()
        .filter(|label| *label != "intake")
        .collect();
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
    let github = sent.iter().filter(|label| **label == "intake").count();
    assert_eq!(
        github, 1,
        "the wizard reads the GitHub sign-in once: {sent:?}"
    );
    let sent: Vec<&str> = sent
        .into_iter()
        .filter(|label| *label != "intake")
        .collect();
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
    publishing_off(&mut shell);
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
    let sent = pump_job(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"job start"), "{sent:?}");
    assert!(sent.contains(&"projects"), "{sent:?}");
    assert!(sent.contains(&"edit pool"), "{sent:?}");
    assert_eq!(
        sent.iter().filter(|sent| **sent == "job dismiss").count(),
        1,
        "{sent:?}"
    );
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

/// Pumps the shell until the engine's job it keeps no longer runs, reading it again between
/// pumps as the run loop does, so a New Project's start, its end and its follow-ups all go
/// out. Takes the shell and its connection. Returns every request sent, oldest first.
async fn pump_job(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sent = pump(shell, connection).await;
    while shell.read_running_job() {
        assert!(
            Instant::now() < deadline,
            "the job never ended: {:?}",
            shell.ctx.store.intake
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        sent.extend(pump(shell, connection).await);
    }
    sent
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
    publishing_off(&mut shell);
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

    let sent = pump_job(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
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
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    let sent = pump_job(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draft_refused_while_another_wizard_is_open_is_kept_ahead_of_that_wizards_draft() {
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
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    pump_job(&mut shell, &mut connection).await;
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready.")
    );

    go_to(&mut shell, "new project");
    pump(&mut shell, &mut connection).await;
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    go_to(&mut shell, "new project");
    assert_eq!(shell.place(), Some(Place::NewProject));
    for character in "Harbor".chars() {
        shell.key(Key::Char(character));
    }
    let sent = pump_job(&mut shell, &mut connection).await;
    assert!(sent.contains(&"job start"), "{sent:?}");
    let refused = shell.ctx.message().expect("the refusal is shown");
    assert!(
        refused.lead.starts_with("Could not create"),
        "{}",
        refused.lead
    );
    assert!(
        refused.rest.contains("Tidepool is kept as a draft"),
        "{}",
        refused.rest
    );
    assert_eq!(shell.place(), Some(Place::NewProject));

    go_to(&mut shell, "home");
    assert_eq!(
        kept_draft().as_deref(),
        Some("Tidepool"),
        "leaving the open wizard keeps its draft behind the refused one, not over it"
    );
    go_to(&mut shell, "new project");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool"), "{shown}");
    assert_eq!(kept_draft().as_deref(), Some("Harbor"));
    go_to(&mut shell, "home");
    assert_eq!(kept_draft().as_deref(), Some("Harbor"));
    go_to(&mut shell, "new project");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Harbor"), "{shown}");
    assert_eq!(kept_draft().as_deref(), Some("Tidepool"));
}

/// Pool Main as the engine lists it, with one Claude account.
fn main_pool() -> PoolSummary {
    PoolSummary {
        id: CredentialRef::new("pool-a").unwrap(),
        name: "Main".into(),
        summary: "1 Claude".into(),
        members: vec![member("a-claude", MemberKind::Claude)],
        project_count: 0,
        provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
    }
}

/// Takes every request the shell queued. Takes the shell. Returns them, oldest first.
fn drain(shell: &mut Shell) -> Vec<Request> {
    std::iter::from_fn(|| shell.ctx.next_request()).collect()
}

/// The intake queries among queued requests. Takes the requests. Returns the queries in
/// order.
fn intake_asks(sent: &[Request]) -> Vec<IntakeQuery> {
    sent.iter()
        .filter_map(|request| match request {
            Request::Intake(job) => Some(job.query()),
            _ => None,
        })
        .collect()
}

/// The New Project job start among queued requests. Takes the requests. Returns the start's
/// id, the project it makes and the query, or none when no such start was queued.
fn setup_start(sent: &[Request]) -> Option<(RequestId, ProjectId, IntakeQuery)> {
    sent.iter().find_map(|request| {
        let Request::Intake(job) = request else {
            return None;
        };
        let query = job.query();
        let IntakeQuery::JobStart {
            id,
            subject: JobSubject::Setup { project_id, .. },
            ..
        } = &query
        else {
            return None;
        };
        Some((id.clone(), project_id.clone(), query.clone()))
    })
}

/// The engine's New Project job for Tidepool as a read shows it. Takes its id, its project
/// and its state. Returns the view.
fn tidepool_job(id: &RequestId, project: &ProjectId, state: JobState) -> JobView {
    JobView {
        id: id.clone(),
        kind: JobKind::Setup,
        project_id: Some(project.clone()),
        source: "Tidepool".into(),
        mode: AddMode::Move,
        overrides: Vec::new(),
        stopping: false,
        state,
    }
}

/// One intake reply from the owner worker. Takes the query it answers and the result.
/// Returns the arrival.
fn intake_reply(
    query: IntakeQuery,
    result: Result<crate::tui::client::intake::IntakeView, String>,
) -> Arrival {
    owner_arrival(OwnerEvent::Intake(Box::new(
        crate::tui::client::intake::IntakeEvent { query, result },
    )))
}

/// A status read that finds this job. Takes the job, or none for no job. Returns the arrival.
fn job_status(job: Option<JobView>) -> Arrival {
    intake_reply(
        IntakeQuery::JobStatus,
        Ok(crate::tui::client::intake::IntakeView::Job(job)),
    )
}

/// The building New Project job a start reply carries. Takes the id and the project.
/// Returns the view.
fn building(id: &RequestId, project: &ProjectId) -> JobView {
    tidepool_job(id, project, JobState::Building { done: 0, total: 0 })
}

/// How many dismisses of this job the requests hold. Takes the requests and the id.
/// Returns the count.
fn dismisses(sent: &[Request], id: &RequestId) -> usize {
    intake_asks(sent)
        .iter()
        .filter(|query| **query == IntakeQuery::JobDismiss { id: id.clone() })
        .count()
}

/// Whether the requests assign pool Main to this project. Takes the requests and the
/// project. Returns true when one such pool edit is queued.
fn assigns_main(sent: &[Request], project: &ProjectId) -> bool {
    sent.iter().any(|request| {
        matches!(
            request,
            Request::EditPool {
                change: crate::tui::seam::PoolChange::Assign {
                    pool: Some(pool),
                    project: assigned,
                },
            } if pool.as_str() == "pool-a" && assigned == project
        )
    })
}

/// Opens New project, gives it pool Main, names the project `name` and presses Enter on
/// Review. Takes the name. Returns the shell and the requests that Enter queued.
fn create_named(name: &str) -> (Shell, Vec<Request>) {
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    shell.arrive(Arrival::Pools(Ok(vec![main_pool()])));
    drain(&mut shell);
    for character in name.chars() {
        shell.key(Key::Char(character));
    }
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    let sent = drain(&mut shell);
    (shell, sent)
}

/// DEC-177 step 3b: Create on New project starts the engine's New Project job with the
/// draft and the picked pool under a fresh id and a new project id, as a move with no
/// overrides; it is the one intake request Enter sends.
#[test]
fn create_starts_the_engines_new_project_job_with_the_draft_and_its_pool() {
    let (shell, sent) = create_named("Tidepool");
    let (id, project, query) = setup_start(&sent).expect("Create starts the New Project job");
    let IntakeQuery::JobStart {
        subject: JobSubject::Setup { spec, setup, .. },
        mode,
        overrides,
        ..
    } = query
    else {
        panic!("the start is a New Project job");
    };
    assert_eq!(spec.name, "Tidepool");
    assert_eq!(
        setup.pool.as_ref().map(CredentialRef::as_str),
        Some("pool-a")
    );
    assert_eq!(mode, AddMode::Move);
    assert!(overrides.is_empty());
    assert!(project.as_str().starts_with("project-"), "{project:?}");
    assert!(!id.as_str().is_empty());
    assert_eq!(intake_asks(&sent).len(), 1, "{sent:?}");
    assert_eq!(shell.place(), Some(Place::NewProject));
}

/// DEC-177 step 3b: the page follows its job by id in the job the shell keeps. Done assigns
/// the picked pool, reads the projects and dismisses the job once; the empty read after
/// that dismiss is not a lost job; the pool's answer opens the project.
#[test]
fn a_done_new_project_job_assigns_its_pool_dismisses_it_once_and_opens_the_project() {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    assert_eq!(shell.place(), Some(Place::NewProject));
    assert_eq!(
        dismisses(&drain(&mut shell), &id),
        0,
        "a running job is kept"
    );

    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    let sent = drain(&mut shell);
    assert!(assigns_main(&sent, &project), "{sent:?}");
    assert!(
        sent.iter()
            .any(|request| matches!(request, Request::Projects)),
        "{sent:?}"
    );
    assert_eq!(dismisses(&sent, &id), 1, "{sent:?}");
    assert_eq!(
        shell.place(),
        Some(Place::NewProject),
        "the pool is assigned first"
    );

    shell.arrive(intake_reply(
        IntakeQuery::JobDismiss { id: id.clone() },
        Ok(crate::tui::client::intake::IntakeView::JobDismissed(true)),
    ));
    shell.arrive(job_status(None));
    let sent = drain(&mut shell);
    assert_eq!(dismisses(&sent, &id), 0, "{sent:?}");
    assert_eq!(shell.place(), Some(Place::NewProject));
    let shown = render(&mut shell, 80, 24);
    assert!(!shown.contains("could not confirm"), "{shown}");

    shell.arrive(Arrival::PoolEdited(Ok(())));
    let Some(Place::Project {
        id: opened,
        name,
        tab,
    }) = shell.place()
    else {
        panic!("the new project opens, got {:?}", shell.place());
    };
    assert_eq!(
        (opened, name.as_str(), tab),
        (project, "Tidepool", ProjectTab::Overview)
    );
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready.")
    );
    assert_eq!(kept_draft(), None);
    assert!(
        setup_start(&drain(&mut shell)).is_none(),
        "nothing starts again"
    );
}

/// DEC-177 step 3b, add.3 decision 4: a New Project sent to the background and dropped by
/// Home still follows its job, assigns its pool, dismisses the job and says it is ready.
#[test]
fn a_new_project_job_run_in_the_background_finishes_after_its_page_is_gone() {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home), "Run in background");
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1, "Home drops the buried wizard");
    drain(&mut shell);

    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    let sent = drain(&mut shell);
    assert!(
        assigns_main(&sent, &project),
        "the picked pool is assigned without the page: {sent:?}"
    );
    assert_eq!(dismisses(&sent, &id), 1, "{sent:?}");

    shell.arrive(intake_reply(
        IntakeQuery::JobDismiss { id: id.clone() },
        Ok(crate::tui::client::intake::IntakeView::JobDismissed(true)),
    ));
    shell.arrive(job_status(None));
    shell.arrive(Arrival::PoolEdited(Ok(())));
    let sent = drain(&mut shell);
    assert!(
        sent.iter()
            .any(|request| matches!(request, Request::PoolProjects)),
        "{sent:?}"
    );
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready.")
    );
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(kept_draft(), None, "a made project keeps no draft");
}

/// DEC-182 S5a, rv-ds241 Note A on a carried page: a New Project sent to the background with
/// publishing on starts its GitHub publish once its pool is assigned, and the Start's
/// refusal is said on Home with its reason.
#[test]
fn a_new_project_run_in_the_background_says_its_github_publish_refusal() {
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    shell.arrive(Arrival::Pools(Ok(vec![main_pool()])));
    drain(&mut shell);
    github_signed_in(&mut shell);
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    let (id, project, start) =
        setup_start(&drain(&mut shell)).expect("Create starts the New Project job");
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1, "Home drops the buried wizard");
    drain(&mut shell);
    shell.arrive(intake_reply(
        start,
        Ok(IntakeView::Job(Some(building(&id, &project)))),
    ));
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    drain(&mut shell);
    shell.arrive(Arrival::PoolEdited(Ok(())));
    let publish: Vec<IntakeQuery> = intake_asks(&drain(&mut shell))
        .into_iter()
        .filter(|query| {
            matches!(query, IntakeQuery::GitHubPublishStart { project: started, .. } if *started == project)
        })
        .collect();
    assert_eq!(publish.len(), 1, "{publish:?}");
    shell.arrive(intake_reply(
        publish[0].clone(),
        Err("github_not_signed_in".into()),
    ));
    let said = shell.ctx.message().expect("the Start's refusal is said");
    assert_eq!(
        (said.lead.as_str(), said.rest.as_str()),
        (
            "Publishing on GitHub didn't start.",
            "Sign in with GitHub first."
        )
    );
    assert_eq!(shell.place(), Some(Place::Home));
}

/// Sends a New Project to the background, drops its page with Home, ends its job done and
/// answers its pool edit. Takes nothing. Returns the shell, the project and the requests the
/// pool's answer queued.
fn made_in_background() -> (Shell, ProjectId, Vec<Request>) {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1, "Home drops the buried wizard");
    drain(&mut shell);
    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    drain(&mut shell);
    shell.arrive(intake_reply(
        IntakeQuery::JobDismiss { id: id.clone() },
        Ok(crate::tui::client::intake::IntakeView::JobDismissed(true)),
    ));
    shell.arrive(job_status(None));
    shell.arrive(Arrival::PoolEdited(Ok(())));
    let sent = drain(&mut shell);
    (shell, project, sent)
}

/// One setup reply from the owner worker. Takes the query it answers and the result.
/// Returns the arrival.
fn setup_reply(query: SetupQuery, result: Result<SetupView, String>) -> Arrival {
    owner_arrival(OwnerEvent::Setup(Box::new(
        crate::tui::client::SetupEvent { query, result },
    )))
}

/// The agent policy writes the requests hold. Takes the requests. Returns the writes.
fn access_writes(sent: &[Request]) -> Vec<SetupQuery> {
    sent.iter()
        .filter_map(|request| match request {
            Request::Setup(
                query @ SetupQuery::Change {
                    action: crate::project_setup::SetupAction::SetAgentPolicy { .. },
                    ..
                },
            ) => Some(query.clone()),
            _ => None,
        })
        .collect()
}

/// DEC-177 add.3 decision 4 (rv-sec2 bgjobs-3b F1): a New Project carried in the background
/// reads its project's agent policy once it is made and saves the picked access at the
/// version read; a refused read or save says the project stays Sandboxed.
#[test]
fn a_new_project_carried_in_the_background_saves_its_agent_access_or_says_it_stays_sandboxed() {
    use super::project_tests::access_view;
    use crate::engine::owner::OwnerResult;
    use crate::sandbox::policy::AgentAccess;
    assert!(
        AgentAccess::NEW_PROJECT.unavailable().is_none(),
        "the wizard preselects a level it can save"
    );
    let sandboxed = "The project is made, but its agents stay Sandboxed.";

    let (mut shell, project, sent) = made_in_background();
    let read = SetupQuery::AgentPolicy {
        project: project.clone(),
    };
    assert_eq!(
        sent.iter()
            .filter(|request| matches!(request, Request::Setup(query) if *query == read))
            .count(),
        1,
        "{sent:?}"
    );
    shell.arrive(setup_reply(
        read,
        Ok(SetupView::AgentPolicy(access_view(
            AgentAccess::Sandboxed,
            7,
        ))),
    ));
    let writes = access_writes(&drain(&mut shell));
    let [write] = writes.as_slice() else {
        panic!("the carried page saves the access once: {writes:?}");
    };
    let SetupQuery::Change {
        action:
            crate::project_setup::SetupAction::SetAgentPolicy {
                project_id,
                version,
                policy,
            },
        ..
    } = write
    else {
        unreachable!("access_writes keeps agent policy writes only");
    };
    assert_eq!(
        (project_id, *version, policy.access),
        (&project, 7, AgentAccess::NEW_PROJECT)
    );
    shell.arrive(setup_reply(
        write.clone(),
        Ok(SetupView::Changed(OwnerResult::Applied)),
    ));
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready."),
        "a saved level adds nothing"
    );
    assert!(access_writes(&drain(&mut shell)).is_empty());

    let (mut shell, project, _) = made_in_background();
    shell.arrive(setup_reply(
        SetupQuery::AgentPolicy { project },
        Ok(SetupView::AgentPolicy(access_view(
            AgentAccess::Sandboxed,
            2,
        ))),
    ));
    let writes = access_writes(&drain(&mut shell));
    let [write] = writes.as_slice() else {
        panic!("the carried page saves the access once: {writes:?}");
    };
    shell.arrive(setup_reply(
        write.clone(),
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "agent_policy_version_conflict".into(),
        })),
    ));
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some(sandboxed),
        "a refused save is said"
    );

    let (mut shell, project, _) = made_in_background();
    shell.arrive(setup_reply(
        SetupQuery::AgentPolicy { project },
        Err("owner_read_failed".into()),
    ));
    assert!(access_writes(&drain(&mut shell)).is_empty());
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some(sandboxed),
        "a refused read is said"
    );
    assert_eq!(shell.place(), Some(Place::Home));
}

/// DEC-177 step 3b: a New Project job that failed keeps the open wizard's answers with the
/// engine's sentence; one stopped in the background keeps the draft and says so in New
/// project's words. Each ended job is dismissed once and assigns no pool.
#[test]
fn a_failed_or_stopped_new_project_job_keeps_the_draft_in_new_project_words() {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Failed {
            code: "home_new_project_failed".into(),
            phase: crate::projects_home::job::Phase::Building,
        },
    ))));
    let sent = drain(&mut shell);
    assert_eq!(dismisses(&sent, &id), 1, "{sent:?}");
    assert!(!assigns_main(&sent, &project), "{sent:?}");
    assert_eq!(shell.place(), Some(Place::NewProject));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Could not create the project."), "{shown}");
    assert!(
        shown.contains("The project folder could not be created. Nothing was kept."),
        "{shown}"
    );

    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Cancelled {
            phase: crate::projects_home::job::Phase::Building,
        },
    ))));
    let sent = drain(&mut shell);
    assert_eq!(dismisses(&sent, &id), 1, "{sent:?}");
    assert!(!assigns_main(&sent, &project), "{sent:?}");
    let said = shell.ctx.message().expect("the stop is said");
    assert_eq!(said.lead, "Setting up the project stopped.");
    assert!(
        said.rest.starts_with("Tidepool is kept as a draft."),
        "{}",
        said.rest
    );
    assert_eq!(kept_draft().as_deref(), Some("Tidepool"));
}

/// DEC-177 step 3b: a start whose reply is lost may have reached the engine, so the page
/// reads the job before it says anything and never starts a second one. A failed read
/// keeps it waiting; a read with its job follows that job to its end; a read without it
/// says the start's own refusal.
#[test]
fn a_new_project_start_with_no_answer_follows_the_job_a_read_finds_and_never_starts_twice() {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.arrive(intake_reply(start, Err("trusted_connection_lost".into())));
    let asks = intake_asks(&drain(&mut shell));
    assert!(asks.contains(&IntakeQuery::JobStatus), "{asks:?}");
    assert!(
        asks.iter()
            .all(|query| !matches!(query, IntakeQuery::JobStart { .. })),
        "{asks:?}"
    );

    shell.arrive(intake_reply(
        IntakeQuery::JobStatus,
        Err("trusted_connection_lost".into()),
    ));
    let shown = render(&mut shell, 80, 24);
    assert!(!shown.contains("Could not create"), "{shown}");
    assert_eq!(shell.place(), Some(Place::NewProject));

    shell.arrive(job_status(Some(building(&id, &project))));
    let shown = render(&mut shell, 80, 24);
    assert!(!shown.contains("Could not create"), "{shown}");
    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    let sent = drain(&mut shell);
    assert!(assigns_main(&sent, &project), "{sent:?}");
    assert!(setup_start(&sent).is_none(), "{sent:?}");

    let (mut shell, sent) = create_named("Tidepool");
    let (id, _, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.arrive(intake_reply(start, Err("project_exists".into())));
    drain(&mut shell);
    shell.arrive(job_status(None));
    let sent = drain(&mut shell);
    assert!(setup_start(&sent).is_none(), "{sent:?}");
    assert_eq!(dismisses(&sent, &id), 0, "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Could not create the folder."), "{shown}");
    assert!(
        shown.contains("A folder for this project already exists."),
        "{shown}"
    );
}

/// DEC-177 step 3b: a status read concerns every New Project left running, so one page that
/// follows its job and another whose start found the slot taken both read it: the first
/// assigns its pool, the second keeps its draft.
#[test]
fn every_new_project_left_running_reads_each_job_status() {
    let (mut shell, sent) = create_named("Tidepool");
    let (id, project, start) = setup_start(&sent).expect("Create starts the New Project job");
    shell.arrive(intake_reply(
        start,
        Ok(crate::tui::client::intake::IntakeView::Job(Some(building(
            &id, &project,
        )))),
    ));
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    go_to(&mut shell, "new project");
    assert_eq!(shell.place(), Some(Place::NewProject));
    drain(&mut shell);
    for character in "Harbor".chars() {
        shell.key(Key::Char(character));
    }
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    let (_, _, harbor) = setup_start(&drain(&mut shell)).expect("Harbor starts its job too");
    shell.key(Key::Esc);
    go_to(&mut shell, "home");
    assert_eq!(shell.depth(), 1);
    shell.arrive(intake_reply(harbor, Err("job_busy".into())));
    drain(&mut shell);

    shell.arrive(job_status(Some(tidepool_job(
        &id,
        &project,
        JobState::Done,
    ))));
    let sent = drain(&mut shell);
    assert!(assigns_main(&sent, &project), "{sent:?}");
    let said = shell.ctx.message().expect("Harbor's refusal is said");
    assert!(
        said.rest.starts_with("Harbor is kept as a draft"),
        "{}",
        said.rest
    );
    assert_eq!(kept_draft().as_deref(), Some("Harbor"));

    shell.arrive(Arrival::PoolEdited(Ok(())));
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Tidepool is ready.")
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
    publishing_off(&mut shell);
    shell.key(Key::ShiftLeft);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool"), "{shown}");
    std::fs::set_permissions(engine.home.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    shell.key(Key::Enter);
    let sent = pump_job(&mut shell, &mut connection).await;
    std::fs::set_permissions(engine.home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sent.first(), Some(&"job start"), "{sent:?}");
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

/// Turns the New project publish block off from the About step, so Create goes on without
/// a GitHub sign-in. Takes the shell with the project name typed. Returns nothing.
fn publishing_off(shell: &mut Shell) {
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    shell.key(Key::Char(' '));
    let shown = render(shell, 80, 24);
    assert!(
        shown.contains("[ ] Publish this project on GitHub"),
        "{shown}"
    );
}

/// Hands the New project page a GitHub sign-in and then the personal account, as the
/// engine answers a signed-in owner. Takes the shell. Returns nothing.
fn github_signed_in(shell: &mut Shell) {
    shell.arrive(owner_arrival(OwnerEvent::Intake(Box::new(IntakeEvent {
        query: IntakeQuery::GitHubStatus,
        result: Ok(IntakeView::GitHub(GitHubStatus {
            signed_in: true,
            login: Some("me".into()),
            method: GitHubMethod::Token,
            waiting: false,
            problem: None,
        })),
    }))));
    let asked: Vec<IntakeQuery> = std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Intake(job) => Some(job.query()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [IntakeQuery::GitHubOwners]);
    shell.arrive(owner_arrival(OwnerEvent::Intake(Box::new(IntakeEvent {
        query: IntakeQuery::GitHubOwners,
        result: Ok(IntakeView::Owners(GitHubOwners {
            owners: vec![GitHubOwner {
                login: "me".into(),
                personal: true,
            }],
            problem: None,
        })),
    }))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_made_project_starts_its_github_publish_once_its_pool_is_assigned_and_says_its_reply() {
    let engine = Engine::start().await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    github_signed_in(&mut shell);
    for character in "Tidepool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::ShiftLeft);
    shell.key(Key::Enter);
    let mut sent = pump_job(&mut shell, &mut connection).await;
    sent.extend(pump(&mut shell, &mut connection).await);
    let at = |label: &str| sent.iter().position(|sent| *sent == label);
    assert!(
        at("job start").is_some()
            && at("job start") < at("edit pool")
            && at("edit pool") < at("github publish"),
        "{sent:?}"
    );
    assert_eq!(
        sent.iter()
            .filter(|label| **label == "github publish")
            .count(),
        1,
        "{sent:?}"
    );
    let message = shell
        .ctx
        .message()
        .expect("the publish Start's reply is said");
    assert_eq!(message.lead, "Publishing on GitHub didn't start.");
    assert_eq!(message.rest, "Sign in with GitHub first.");
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
        landing_result: crate::landing::LandingResult::None,
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

mod overview_tabs {
    use crate::project_overview::{
        Apply, Competitors, Overview, Readme, TAB_BYTES_MAX, Tab, TabState,
    };
    use crate::protocol::{CredentialRef, ProjectId};
    use crate::roadmap_status::Refresh;
    use crate::tui::frame::{Hint, footer, hint};
    use crate::tui::glyphs::UNICODE;
    use crate::tui::screens::new_project_view::{Look, Row};
    use crate::tui::screens::project::{
        OVERVIEW_TAB_LABELS, PoolInfo, ProjectReply, ProjectScreen,
    };
    use crate::tui::screens::project_publish::PROMPT_LINE;
    use crate::tui::screens::project_view::project_screen;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const TAB_ROW: &str = "Quick info  Features  Competitors  Install & use  README";

    fn empty() -> Overview {
        Overview {
            readme: None,
            tabs: Tab::ALL
                .into_iter()
                .map(|tab| TabState {
                    tab,
                    text: None,
                    version: String::new(),
                    edited: false,
                    foreign: false,
                    proposal: None,
                    proposal_version: String::new(),
                    blocked: None,
                })
                .collect(),
            competitors: Competitors::Missing,
            running: false,
            stale: false,
            checked_ms: None,
            failure: None,
            waiting_for_account: false,
            due_in_ms: None,
            refresh: Refresh::AfterLanding,
            apply: Apply::AlwaysAsk,
            needs_home: false,
        }
    }

    fn overview_with(tab: Tab, text: &str) -> Overview {
        let mut overview = empty();
        let state = overview.tabs.iter_mut().find(|state| state.tab == tab);
        state.unwrap().text = Some(text.to_owned());
        overview
    }

    fn page_with_overview(overview: Overview) -> ProjectScreen {
        let project = ProjectId::new("alpha").unwrap();
        let mut page = ProjectScreen::new(project, "Alpha".into(), None);
        page.overview_tabs = Some(overview);
        page
    }

    fn press(page: &mut ProjectScreen, code: KeyCode) -> ProjectReply {
        page.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn frame(page: &mut ProjectScreen) -> String {
        let drawn = project_screen(page, 100, 40, &Look::plain());
        let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
        rows.join("\n")
    }

    fn flat(page: &mut ProjectScreen) -> String {
        let shown = frame(page);
        shown.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn blocked(tab: Tab, code: &str) -> Overview {
        let mut overview = overview_with(tab, "Old text that must not show.\n");
        for state in &mut overview.tabs {
            if state.tab == tab || code == "not_a_folder" {
                state.text = None;
                state.blocked = Some(code.to_owned());
            }
        }
        overview
    }

    fn marked(label: &str) -> String {
        TAB_ROW.replace(label, &format!("[{label}]"))
    }

    #[test]
    fn the_overview_shows_the_tab_names_and_the_picked_tabs_text() {
        let mut page = page_with_overview(overview_with(Tab::Features, "- Search papers\n"));
        assert!(frame(&mut page).contains(&marked("Quick info")));
        assert_eq!(press(&mut page, KeyCode::Right), ProjectReply::Handled);
        let shown = frame(&mut page);
        assert!(shown.contains("Quick info  [Features]  Competitors  Install & use  README"));
        assert!(shown.contains("Search papers"));
        assert_eq!(OVERVIEW_TAB_LABELS.join("  "), TAB_ROW);
    }

    #[test]
    fn left_and_right_move_the_picked_tab_and_wrap_at_both_ends() {
        let mut overview = overview_with(Tab::QuickInfo, "Alpha reads papers.\n");
        overview.readme = Some(Readme {
            path: "README.md".into(),
            text: "# Alpha\n\nInstall with cargo.\n".into(),
            cut: true,
        });
        let mut page = page_with_overview(overview);
        assert!(frame(&mut page).contains("Alpha reads papers."));
        assert_eq!(press(&mut page, KeyCode::Left), ProjectReply::Handled);
        let readme = frame(&mut page);
        assert!(readme.contains(&marked("README")));
        assert!(readme.contains("README.md"));
        assert!(readme.contains("This is the project's own file."));
        assert!(readme.contains("Showing the first part of a long file."));
        assert!(readme.contains("Install with cargo."));
        assert!(!readme.contains("Alpha reads papers."));
        press(&mut page, KeyCode::Right);
        assert!(frame(&mut page).contains(&marked("Quick info")));
        for label in ["Features", "Competitors", "Install & use", "README"] {
            press(&mut page, KeyCode::Right);
            assert!(frame(&mut page).contains(&marked(label)), "{label}");
        }
        assert!(page.overview_tab < OVERVIEW_TAB_LABELS.len());
    }

    #[test]
    fn a_project_without_a_readme_says_so() {
        let mut page = page_with_overview(empty());
        press(&mut page, KeyCode::Left);
        assert!(frame(&mut page).contains("This project has no README file."));
    }

    #[test]
    fn a_waiting_proposal_points_to_the_desktop_and_only_on_its_own_tab() {
        let mut overview = overview_with(Tab::Features, "- Search papers\n");
        overview.tabs[1].proposal = Some("- Search papers\n- Share lists\n".into());
        let mut page = page_with_overview(overview);
        let line = "A refresh proposes changes. Review them in the desktop app.";
        assert!(!frame(&mut page).contains(line));
        press(&mut page, KeyCode::Right);
        let shown = frame(&mut page);
        assert!(shown.contains(line));
        assert!(shown.contains("Search papers"));
        assert!(!shown.contains("Share lists"));
    }

    /// RV-DS241-2: a tab whose file came with the project's files, not written by the engine
    /// nor saved by the owner, says so above its text in the desktop's words (DEC-141), and
    /// only on that tab; a tab the owner edited does not.
    #[test]
    fn a_foreign_tab_file_says_it_came_with_the_projects_files() {
        let line = concat!(
            "This tab's file came with the project's files: CrazyAgent didn't write it and ",
            "you didn't save it. Check it before you use its commands."
        );
        let mut overview = overview_with(Tab::InstallAndUse, "Run setup.sh first.\n");
        overview.tabs[3].foreign = true;
        let mut page = page_with_overview(overview.clone());
        assert!(!flat(&mut page).contains(line), "only on its own tab");
        for _ in 0..3 {
            press(&mut page, KeyCode::Right);
        }
        assert!(frame(&mut page).contains(&marked("Install & use")));
        let shown = flat(&mut page);
        assert!(shown.contains(line), "{shown}");
        assert!(shown.contains("Run setup.sh first."));
        overview.tabs[3].foreign = false;
        overview.tabs[3].edited = true;
        let mut mine = page_with_overview(overview);
        for _ in 0..3 {
            press(&mut mine, KeyCode::Right);
        }
        assert!(
            !flat(&mut mine).contains(line),
            "the owner's edit is not foreign"
        );
    }

    #[test]
    fn a_tab_not_written_yet_says_what_will_write_it() {
        let shown = |change: fn(&mut Overview)| {
            let mut overview = empty();
            change(&mut overview);
            let mut page = page_with_overview(overview);
            press(&mut page, KeyCode::Right);
            frame(&mut page)
        };
        assert!(
            shown(|_| ())
                .contains("Not written yet. It is written after the next landed change set")
        );
        assert!(shown(|o| o.refresh = Refresh::RefreshOnly).contains("Not written yet. It is"));
        assert!(
            !shown(|o| o.refresh = Refresh::RefreshOnly).contains("landed change set"),
            "Refresh only never writes after a landing"
        );
        assert!(
            shown(|o| o.refresh = Refresh::Off)
                .contains("Overview tabs are off in Project settings.")
        );
        let running = shown(|o| o.running = true);
        assert!(running.contains("Writing now"));
        assert!(!running.contains("Not written yet"));
    }

    #[test]
    fn a_failed_run_shows_its_reason_and_keeps_the_tab() {
        let mut overview = overview_with(Tab::QuickInfo, "Alpha reads papers.\n");
        overview.failure = Some("The account is out of usage".into());
        let shown = frame(&mut page_with_overview(overview));
        assert!(shown.contains("Couldn't refresh the tabs. The account is out of usage"));
        assert!(shown.contains("Alpha reads papers."));
    }

    #[test]
    fn a_run_waiting_for_an_account_says_why_and_never_that_the_refresh_failed() {
        let reason = crate::project_overview::NO_ACCOUNT;
        for (stale, running) in [(true, false), (false, true)] {
            let mut overview = empty();
            overview.failure = Some(reason.into());
            overview.waiting_for_account = true;
            overview.stale = stale;
            overview.running = running;
            let shown = flat(&mut page_with_overview(overview));
            assert!(shown.contains(reason), "{shown}");
            assert!(!shown.contains("Couldn't refresh the tabs"), "{shown}");
        }
        let mut ended = empty();
        ended.failure = Some(reason.into());
        ended.waiting_for_account = true;
        let shown = flat(&mut page_with_overview(ended));
        assert!(
            shown.contains(&format!("Couldn't refresh the tabs. {reason}")),
            "{shown}"
        );
    }

    /// An ended real failure stays a failed refresh while the next run waits to start and
    /// while it writes (IW-L1, DEC-147 addendum): only the engine's wait for an account is
    /// shown as a wait, in the same words as the desktop app.
    #[test]
    fn an_ended_failure_stays_a_failed_refresh_while_a_new_run_waits_or_writes() {
        let reason = crate::project_overview::UNFINISHED;
        for (stale, running) in [(true, false), (false, true), (true, true)] {
            let mut overview = empty();
            overview.failure = Some(reason.into());
            overview.stale = stale;
            overview.running = running;
            let shown = flat(&mut page_with_overview(overview));
            assert!(
                shown.contains(&format!("Couldn't refresh the tabs. {reason}")),
                "stale {stale} running {running}: {shown}"
            );
        }
    }

    #[test]
    fn before_the_tabs_are_read_there_is_no_tab_row_and_the_arrows_do_nothing() {
        let project = ProjectId::new("alpha").unwrap();
        let mut page = ProjectScreen::new(project, "Alpha".into(), None);
        assert!(!frame(&mut page).contains("Install & use"));
        assert_eq!(press(&mut page, KeyCode::Right), ProjectReply::Ignored);
        assert_eq!(press(&mut page, KeyCode::PageDown), ProjectReply::Ignored);
        assert_eq!(page.overview_tab, 0);
    }

    #[test]
    fn a_long_tab_scrolls_with_page_keys_and_says_when_more_is_below() {
        let lines: Vec<String> = (1..=120).map(|n| format!("line number {n}")).collect();
        let mut page = page_with_overview(overview_with(Tab::QuickInfo, &lines.join("\n\n")));
        let top = frame(&mut page);
        assert!(top.contains("line number 1"));
        assert!(!top.contains("line number 120"));
        assert!(top.contains("PgDn for more"));
        for _ in 0..40 {
            assert_eq!(press(&mut page, KeyCode::PageDown), ProjectReply::Handled);
        }
        let end = frame(&mut page);
        assert!(
            end.contains("line number 120"),
            "the scroll stops at the last line"
        );
        assert!(!end.contains("PgDn for more"));
        for _ in 0..40 {
            press(&mut page, KeyCode::PageUp);
        }
        assert_eq!(page.overview_scroll, 0);
        press(&mut page, KeyCode::PageDown);
        press(&mut page, KeyCode::Right);
        assert_eq!(page.overview_scroll, 0, "a tab opens at its top");
    }

    #[test]
    fn hidden_characters_of_a_tab_show_as_visible_escapes() {
        let text = "Zebra\u{202e} reads\u{1b}[31m papers.\n";
        let shown = frame(&mut page_with_overview(overview_with(Tab::QuickInfo, text)));
        assert!(!shown.contains('\u{202e}'));
        assert!(!shown.contains('\u{1b}'));
        let escaped = format!("Zebra{}u202e reads papers.", '\\');
        assert!(shown.contains(&escaped), "{shown}");
    }

    #[test]
    fn a_project_with_a_pool_shows_the_tab_row_beside_its_team() {
        let mut page = page_with_overview(overview_with(Tab::QuickInfo, "Zebra reads papers.\n"));
        page.pool = Some(PoolInfo {
            id: CredentialRef::new("pool-main").unwrap(),
            name: "Main".into(),
            accounts: 1,
            assigned: true,
        });
        let shown = frame(&mut page);
        assert!(shown.contains(&marked("Quick info")), "{shown}");
        assert!(shown.contains("Zebra reads papers."), "{shown}");
        assert!(shown.contains("Team"), "{shown}");
        assert!(!shown.contains("This project has no pool"), "{shown}");
        let help = project_screen(&mut page, 100, 40, &Look::plain()).help;
        assert!(help.iter().any(|(_, what)| *what == "Overview tab"));
        assert!(help.iter().any(|(_, what)| *what == "Scroll"));
    }

    #[test]
    fn an_80_by_24_overview_keeps_the_publish_question_and_the_keys_it_had_before() {
        let text: String = (1..=12).map(|line| format!("- Point {line}\n")).collect();
        let mut overview = overview_with(Tab::QuickInfo, &text);
        overview.failure = Some(
            "The agent stopped before it wrote the tabs, so they show what they said the last time they were read. "
                .repeat(3),
        );
        let mut page = page_with_overview(overview);
        page.pool = Some(PoolInfo {
            id: CredentialRef::new("pool-main").unwrap(),
            name: "Main".into(),
            accounts: 1,
            assigned: true,
        });
        page.github_notice = true;
        let drawn = project_screen(&mut page, 80, 24, &Look::plain());
        let shown = drawn
            .rows
            .iter()
            .map(Row::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("Main  1 account"), "pooled: {shown}");
        let notice = shown.find(PROMPT_LINE);
        let strip = shown.find(&marked("Quick info"));
        assert!(notice.is_some(), "the question shows at 80x24: {shown}");
        assert!(strip.is_some(), "{shown}");
        assert!(
            notice < strip,
            "the question sits above the tabs, so their rows can't push it off: {shown}"
        );
        assert!(
            shown.contains("G Publish on GitHub"),
            "the body line names G: {shown}"
        );
        let hints: Vec<Hint> = drawn
            .help
            .iter()
            .map(|(key, label)| hint(key, label))
            .collect();
        let ctx = super::ctx(80, 24);
        let bar = footer(80, &hints, 0, &ctx.theme, &UNICODE);
        let bar: String = bar.spans.iter().map(|span| span.content.as_ref()).collect();
        for key in ["Enter Open", "o Open folder", "PgUp/PgDn Scroll"] {
            assert!(bar.contains(key), "{key} stays in the bar: {bar}");
        }
    }

    #[test]
    fn the_key_help_names_the_tab_and_scroll_keys_once_the_tabs_are_read() {
        let mut page = page_with_overview(empty());
        let help = project_screen(&mut page, 100, 40, &Look::plain()).help;
        assert!(help.iter().any(|(_, what)| *what == "Overview tab"));
        assert!(help.contains(&("Shift-R", "Refresh tabs")));
        let project = ProjectId::new("alpha").unwrap();
        let mut unread = ProjectScreen::new(project, "Alpha".into(), None);
        let help = project_screen(&mut unread, 100, 40, &Look::plain()).help;
        assert!(!help.iter().any(|(_, what)| *what == "Overview tab"));
    }

    #[test]
    fn a_blocked_tab_says_why_names_its_file_and_what_to_do() {
        let large = format!("its file is larger than {} KB", TAB_BYTES_MAX / 1024);
        let reasons = [
            ("blank", "its file is empty"),
            ("too_large", large.as_str()),
            ("not_utf8", "its file is not plain text"),
            ("something_new", "its file can't be read"),
        ];
        assert_eq!(TAB_BYTES_MAX % 1024, 0, "the bound reads as whole KB");
        for (code, why) in reasons {
            let mut page = page_with_overview(blocked(Tab::Features, code));
            press(&mut page, KeyCode::Right);
            let shown = flat(&mut page);
            let line = format!(
                "CrazyAgent can't show this tab: {why}. The file is \
                 .crazyagent/overview/features.md in the project. Fix or delete the file, \
                 then refresh the tabs."
            );
            assert!(shown.contains(&line), "{code}\n{shown}");
            assert!(!shown.contains("Not written yet"), "{code}\n{shown}");
            assert!(!shown.contains("something_new"), "{code}\n{shown}");
            press(&mut page, KeyCode::Left);
            assert!(!flat(&mut page).contains("CrazyAgent can't show this tab"));
        }
    }

    #[test]
    fn a_link_or_folder_in_a_tab_files_place_asks_to_remove_it() {
        let mut page = page_with_overview(blocked(Tab::InstallAndUse, "not_a_file"));
        for _ in 0..3 {
            press(&mut page, KeyCode::Right);
        }
        let shown = flat(&mut page);
        let line = "CrazyAgent can't show this tab: a link or folder is where its file goes. \
                    The path is .crazyagent/overview/install-and-use.md in the project. \
                    Remove it, then refresh the tabs.";
        assert!(shown.contains(line), "{shown}");
        assert!(!shown.contains("Fix or delete the file"), "{shown}");
    }

    #[test]
    fn a_tabs_folder_that_is_none_blocks_every_generated_tab_and_names_the_folder() {
        let mut page = page_with_overview(blocked(Tab::QuickInfo, "not_a_folder"));
        let line = "CrazyAgent can't show this tab: the folder for the tabs is a link or a file. \
                    The folder is .crazyagent/overview in the project. \
                    Remove it, then refresh the tabs.";
        for _ in 0..Tab::ALL.len() {
            let shown = flat(&mut page);
            assert!(shown.contains(line), "{shown}");
            assert!(!shown.contains(".md in the project"), "{shown}");
            press(&mut page, KeyCode::Right);
        }
        assert!(flat(&mut page).contains("This project has no README file."));
    }

    #[test]
    fn a_project_outside_the_crazyagent_folder_says_the_tabs_need_it_there_and_keeps_the_readme() {
        let mut overview = empty();
        overview.needs_home = true;
        overview.readme = Some(Readme {
            path: "README.md".into(),
            text: "Zebra reads papers.\n".into(),
            cut: false,
        });
        let mut page = page_with_overview(overview);
        let line = "Overview tabs need this project in the CrazyAgent folder. \
                    Move it there in the desktop app, in Project settings.";
        for _ in 0..Tab::ALL.len() {
            let shown = flat(&mut page);
            assert!(shown.contains(line), "{shown}");
            assert!(!shown.contains("Not written yet"), "{shown}");
            press(&mut page, KeyCode::Right);
        }
        let readme = flat(&mut page);
        assert!(readme.contains("Zebra reads papers."), "{readme}");
        assert!(
            !readme.contains("Overview tabs need this project"),
            "{readme}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_requested_memory_is_approved_from_the_project_context_tab_on_a_real_engine() {
    use crate::memory::library::{AgentSaved, MemoryOrigin, MemoryScope, agent_remember};
    let mut engine = Engine::start().await;
    let notes = engine.project("notes-cli", "Notes CLI", 2).await;
    {
        let mut db = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.execute(
            "INSERT INTO memory_settings(singleton,agent_writes) VALUES(1,'ask')
             ON CONFLICT(singleton) DO UPDATE SET agent_writes=excluded.agent_writes",
            [],
        )
        .unwrap();
        assert_eq!(
            agent_remember(&mut db, &notes, "Ada (@lead-a)", false, "Ship on Fridays.").unwrap(),
            Ok(AgentSaved {
                waiting: true,
                narrowed: false
            }),
            "under Ask me first the agent's memory waits for the owner"
        );
    }
    let mut connection = engine.connect();
    let mut ctx = ctx(120, 40);
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Context,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    pump(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("p opens Project memory."), "{shown}");

    shell.key(Key::Char('p'));
    assert_eq!(
        shell.place(),
        Some(Place::ProjectMemory {
            id: notes.clone(),
            name: "Notes CLI".into(),
        })
    );
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"settings"), "{sent:?}");
    let shown = render(&mut shell, 120, 40);
    assert!(shown.contains("Notes CLI  /  Project memory"), "{shown}");
    assert!(
        shown.contains("1 memory waits for your approval."),
        "{shown}"
    );
    assert!(shown.contains("Ship on Fridays."), "{shown}");
    assert!(shown.contains("Needs approval"), "{shown}");
    assert!(!shown.contains("Agent memory writes"), "{shown}");

    shell.key(Key::Char('y'));
    let sent = pump(&mut shell, &mut connection).await;
    assert!(sent.contains(&"settings"), "{sent:?}");
    let shown = render(&mut shell, 120, 40);
    assert!(
        shown.contains("Memory approved. Agents get it from their next launch."),
        "{shown}"
    );
    assert!(
        shown.contains("Nothing waits for your approval."),
        "{shown}"
    );

    let page = engine
        .controller
        .memories(
            MemoryScope::Project {
                project_id: notes.clone(),
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].origin,
        MemoryOrigin::Agent {
            agent: "Ada (@lead-a)".into()
        }
    );

    shell.key(Key::Esc);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Context,
        })
    );
}

/// A delete an engine stop cut short shows on the project's banner in plain words, with Ctrl-D
/// Keep the project, which clears the mark, and Ctrl-X Delete again, which opens the remove ask
/// with Delete everything picked once the engine's preview offers it (DEC-141, DEC-118).
#[test]
fn a_delete_cut_short_offers_keep_the_project_and_delete_again_on_the_banner() {
    use crate::engine::owner::{BannerNotice, RecoveryBanner};
    use crate::tui::client::SetupEvent;
    use crate::tui::client::recovery::{RecoveryEvent, RecoveryQuery, RecoveryReply};
    let notes = ProjectId::new("notes-cli").unwrap();
    let mut ctx = ctx(110, 40);
    ctx.link = crate::tui::ctx::Link::Ready;
    let screen = open(
        &Place::Project {
            id: notes.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let asked = |shell: &mut Shell| {
        let mut recovery = Vec::new();
        let mut setup = Vec::new();
        while let Some(request) = shell.ctx.next_request() {
            match request {
                Request::Recovery(query) => recovery.push(*query),
                Request::Setup(query) => setup.push(query),
                _ => {}
            }
        }
        (recovery, setup)
    };
    let (read, first) = asked(&mut shell);
    assert_eq!(
        read,
        [RecoveryQuery::Read {
            project: notes.clone()
        }]
    );
    for query in first {
        shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
            SetupEvent {
                query,
                result: Err("project_setup_missing".into()),
            },
        )))));
    }
    let notice: BannerNotice = serde_json::from_value(serde_json::json!({
        "action_kind": "project_delete",
        "id": 9,
        "agent": null,
        "label": null,
        "change_set": null,
        "shown": "unknown",
        "started_ms": 1_800_000_000_000_i64,
    }))
    .expect("the engine names a delete cut short");
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Recovery(Box::new(
        RecoveryEvent {
            query: read[0].clone(),
            result: Ok(RecoveryReply::Banner(RecoveryBanner {
                runs: Vec::new(),
                notices: vec![notice.clone()],
                more: 0,
            })),
        },
    )))));
    let out = render(&mut shell, 110, 40);
    for words in [
        "A delete was cut short. Some folders may already be in the Trash.",
        "Ctrl-D Keep the project",
        "Ctrl-X Delete again",
    ] {
        assert!(out.contains(words), "{words:?} is drawn:\n{out}");
    }

    shell.key(Key::Ctrl('x'));
    let (recovery, setup) = asked(&mut shell);
    assert!(recovery.is_empty(), "Delete again clears nothing by itself");
    let removal: Vec<SetupQuery> = setup
        .into_iter()
        .filter(|query| matches!(query, SetupQuery::Removal { .. }))
        .collect();
    assert_eq!(
        removal,
        [SetupQuery::Removal {
            project: notes.clone()
        }],
        "Delete again reads what removing would do, as x does"
    );
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Setup(Box::new(
        SetupEvent {
            query: removal[0].clone(),
            result: Ok(SetupView::Removal(Box::new(
                crate::team::cleanup::RemovalPreview {
                    name: "Notes CLI".into(),
                    folders: vec!["/Users/owner/CrazyAgent/notes-cli".into()],
                    can_delete: true,
                    blocked: None,
                    running: Vec::new(),
                    open_change_sets: 0,
                    losses: Vec::new(),
                },
            ))),
        },
    )))));
    let ask = render(&mut shell, 110, 40);
    assert!(
        ask.contains("[ Delete everything ]") && ask.contains("Type the project name:"),
        "the remove ask opens on Delete everything:\n{ask}"
    );
    shell.key(Key::Esc);
    let kept = render(&mut shell, 110, 40);
    assert!(
        !kept.contains("[ Delete everything ]"),
        "Esc keeps the project:\n{kept}"
    );

    shell.key(Key::Ctrl('d'));
    let (recovery, _) = asked(&mut shell);
    assert_eq!(recovery.len(), 1, "Keep the project sends one dismiss");
    let RecoveryQuery::Dismiss { project, kind, id } = &recovery[0] else {
        panic!("Keep the project dismisses the notice: {recovery:?}");
    };
    assert_eq!(project, &notes);
    assert_eq!(serde_json::to_value(kind).unwrap(), "project_delete");
    assert_eq!(*id, 9);
}

fn publish_job(step: GitHubPublishStep) -> GitHubPublishJob {
    GitHubPublishJob {
        project: std::path::PathBuf::from("/p"),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: "tool".into(),
            description: String::new(),
            private: true,
            docs: false,
        },
        lines: Vec::new(),
        written: Vec::new(),
        step,
        stopped: None,
        made: None,
        rules: crate::github::GitHubRule::defaults(),
    }
}

/// Takes the GitHub publish queries the page queued, oldest first, dropping every other
/// request. Takes the shell. Returns the queries.
fn publish_asks(shell: &mut Shell) -> Vec<IntakeQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Intake(job) => Some(job.query()),
            _ => None,
        })
        .filter(|query| {
            matches!(
                query,
                IntakeQuery::GitHubPublishStatus { .. }
                    | IntakeQuery::GitHubPublishPrompt { .. }
                    | IntakeQuery::GitHubPublishPromptSet { .. }
                    | IntakeQuery::GitHubPublishStart { .. }
                    | IntakeQuery::GitHubStatus
                    | IntakeQuery::GitHubOwners
                    | IntakeQuery::GitHubPublishApprove { .. }
                    | IntakeQuery::GitHubPublishExclude { .. }
                    | IntakeQuery::GitHubPublishRecheck { .. }
                    | IntakeQuery::GitHubPublishContinue { .. }
                    | IntakeQuery::GitHubPublishDiscard { .. }
            )
        })
        .collect()
}

fn publish_reply(shell: &mut Shell, query: IntakeQuery, result: Result<IntakeView, String>) {
    shell.arrive(owner_arrival(OwnerEvent::Intake(Box::new(IntakeEvent {
        query,
        result,
    }))));
}

#[test]
fn the_overview_reads_the_github_publish_and_its_pane_sends_the_owners_answer() {
    let mut ctx = ctx(100, 30);
    let project = ProjectId::new("notes").unwrap();
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status.clone(),
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Lines {
                lines: vec![".env*".into(), "target/".into()],
                draft: GitHubIgnoreDraft::Agent,
            },
        ))))),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("Publish on GitHub"), "{shown}");
    assert!(shown.contains("[x] target/"), "{shown}");
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let approve = IntakeQuery::GitHubPublishApprove {
        project: project.clone(),
        lines: vec![".env*".into()],
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&approve));
    shell.key(Key::Enter);
    assert!(publish_asks(&mut shell).is_empty(), "one answer at a time");
    publish_reply(
        &mut shell,
        approve,
        Ok(IntakeView::Publish(Box::new(publish_job(
            GitHubPublishStep::Done {
                full_name: "me/tool".into(),
                outcomes: Vec::<GitHubRuleOutcome>::new(),
            },
        )))),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("Published as me/tool."), "{shown}");
    shell.key(Key::Esc);
    let shown = render(&mut shell, 100, 30);
    assert!(!shown.contains("Published as me/tool."), "{shown}");
    assert!(publish_asks(&mut shell).is_empty());
}

#[test]
fn a_project_without_a_publish_shows_no_pane_and_a_refused_answer_is_said() {
    let project = ProjectId::new("notes").unwrap();
    let opened = |project: &ProjectId| {
        let mut context = ctx(100, 30);
        let screen = open(
            &Place::Project {
                id: project.clone(),
                name: "Notes".into(),
                tab: ProjectTab::Overview,
            },
            &mut context,
        );
        Shell::with(context, screen)
    };
    let mut shell = opened(&project);
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status.clone(),
        Ok(IntakeView::PublishStatus(None)),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(!shown.contains("Publish on GitHub"), "{shown}");
    let mut shell = opened(&project);
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status,
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Lines {
                lines: vec![".env*".into()],
                draft: GitHubIgnoreDraft::Agent,
            },
        ))))),
    );
    shell.key(Key::Enter);
    let approve = IntakeQuery::GitHubPublishApprove {
        project,
        lines: vec![".env*".into()],
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&approve));
    publish_reply(&mut shell, approve, Err("github_publish_running".into()));
    let message = shell.ctx.message().expect("the refusal is said");
    assert_eq!(message.lead, "The publish answer wasn't taken.");
    assert_eq!(message.rest, super::intake::copy("github_publish_running"));
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("[x] .env*"), "the pane stays: {shown}");
}

/// Opens the Overview of project `notes`, named Notes, and answers its publish read with
/// no publish. Takes nothing. Returns the shell and the project.
fn unpublished() -> (Shell, ProjectId) {
    let mut ctx = ctx(100, 30);
    let project = ProjectId::new("notes").unwrap();
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    (shell, project)
}

/// Opens the same Overview with the engine saying to offer a publish. Takes nothing.
/// Returns the shell and the project.
fn offered() -> (Shell, ProjectId) {
    let (mut shell, project) = unpublished();
    let prompt = IntakeQuery::GitHubPublishPrompt {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(true)));
    (shell, project)
}

#[test]
fn a_project_with_no_publish_shows_the_question_on_its_overview_and_keeps_not_now() {
    let (mut shell, project) = offered();
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains(super::project_publish::PROMPT_LINE),
        "{shown}"
    );
    assert!(shown.contains("G Publish on GitHub"), "{shown}");
    shell.key(Key::Char('G'));
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("x Don't ask again"), "{shown}");
    shell.key(Key::Char('n'));
    let kept = IntakeQuery::GitHubPublishPromptSet {
        project: project.clone(),
        answer: crate::github::GitHubPromptAnswer::NotNow,
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&kept));
    shell.key(Key::Char('x'));
    assert!(publish_asks(&mut shell).is_empty(), "one answer at a time");
    publish_reply(&mut shell, kept, Ok(IntakeView::PromptKept));
    let shown = render(&mut shell, 100, 30);
    assert!(
        !shown.contains(super::project_publish::PROMPT_LINE),
        "{shown}"
    );
    assert!(!shown.contains("Don't ask again"), "{shown}");
    shell.key(Key::Char('G'));
    assert!(!render(&mut shell, 100, 30).contains("Don't ask again"));
    let (mut quiet, project) = unpublished();
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut quiet), std::slice::from_ref(&prompt));
    publish_reply(&mut quiet, prompt, Err("github_store_unavailable".into()));
    assert!(
        quiet.ctx.message().is_none(),
        "a failed question read stays quiet"
    );
    let shown = render(&mut quiet, 100, 30);
    assert!(
        !shown.contains(super::project_publish::PROMPT_LINE),
        "{shown}"
    );
}

#[test]
fn publish_from_the_overview_question_starts_the_publish_the_form_shows_and_says_so() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    shell.key(Key::Esc);
    assert!(publish_asks(&mut shell).is_empty(), "Esc starts nothing");
    let shown = render(&mut shell, 100, 30);
    assert!(!shown.contains("[ Publish ]"), "{shown}");
    assert!(
        shown.contains(super::project_publish::PROMPT_LINE),
        "the question stays: {shown}"
    );
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("[ Publish ]"), "{shown}");
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: crate::github::default_repo_name("Notes"),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert!(
        publish_asks(&mut shell).is_empty(),
        "the question is gone once a publish starts"
    );
    publish_reply(
        &mut shell,
        start,
        Ok(IntakeView::Publish(Box::new(publish_job(
            GitHubPublishStep::Drafting,
        )))),
    );
    let message = shell.ctx.message().expect("the start is said");
    assert_eq!(message.lead, super::new_project_publish::PUBLISH_STARTED);
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains("Drafting the .gitignore lines..."),
        "{shown}"
    );
}

#[test]
fn a_refused_publish_from_the_question_says_why_and_asks_again_at_once() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: crate::github::default_repo_name("Notes"),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    shell.key(Key::Esc);
    assert!(
        matches!(shell.place(), Some(Place::Project { .. })),
        "Esc closes the form the Start went from, not the page: {:?}",
        shell.place()
    );
    publish_reply(&mut shell, start, Err("github_publish_in_progress".into()));
    let message = shell.ctx.message().expect("the refusal is said");
    assert_eq!(
        message.lead,
        super::new_project_publish::PUBLISH_NOT_STARTED
    );
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(
        publish_asks(&mut shell),
        std::slice::from_ref(&status),
        "a refused Start reads the publish again at once"
    );
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(true)));
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains(super::project_publish::PROMPT_LINE),
        "the question is back: {shown}"
    );
}

#[test]
fn a_refused_publish_from_the_question_keeps_the_form_with_what_was_typed_and_says_why_in_it() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Paste("-2".into()));
    for _ in 0..3 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let name = format!("{}-2", crate::github::default_repo_name("Notes"));
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: name.clone(),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains("Starting the publish..."),
        "the form waits on the Start: {shown}"
    );
    shell.key(Key::Enter);
    assert!(publish_asks(&mut shell).is_empty(), "one Start at a time");
    publish_reply(
        &mut shell,
        start.clone(),
        Err("github_repo_name_taken".into()),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("[ Publish ]"), "the form stays: {shown}");
    assert!(shown.contains(&name), "the typed name stays: {shown}");
    assert!(
        shown.contains("Pick another name."),
        "the form says why: {shown}"
    );
    assert!(
        !shown.contains("Sending your answer...") && shown.contains("Checking..."),
        "the read after the refusal sends no answer: {shown}"
    );
    assert!(
        shell.ctx.message().is_none(),
        "the form says why, so the message line stays quiet"
    );
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(false)));
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains("[ Publish ]") && shown.contains(&name),
        "a form the owner opened stays when the engine stops asking: {shown}"
    );
    assert!(shown.contains("Pick another name."), "{shown}");
    shell.key(Key::Enter);
    assert_eq!(
        publish_asks(&mut shell),
        [start],
        "Publish again sends the kept plan"
    );
}

/// RV-DS241 S5b TUI nit 2: the Overview's publish question and a refused publish form, its
/// three-line reason included, fit an 80x24 window with every key in the bar.
#[test]
fn the_publish_question_and_a_refused_form_fit_an_80_by_24_window() {
    let (mut shell, project) = offered();
    shell.ctx.width = 80;
    shell.ctx.height = 24;
    shell.key(Key::Char('G'));
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains(super::project_publish::PROMPT_LINE),
        "{shown}"
    );
    let bar = shown.lines().last().unwrap_or_default().to_owned();
    for key in [
        "Enter Publish",
        "n Not now",
        "x Don't ask again",
        "Esc Close",
    ] {
        assert!(bar.contains(key), "{key}: {bar}");
    }
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: crate::github::default_repo_name("Notes"),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    publish_reply(
        &mut shell,
        start,
        Err("github_publish_secret_in_history".into()),
    );
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(true)));
    let shown = render(&mut shell, 80, 24);
    for text in [
        "Visibility",
        "Repository",
        "[ Publish ]",
        "Publishing on GitHub didn't start.",
        "then check again.",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
    let bar = shown.lines().last().unwrap_or_default().to_owned();
    for key in ["Enter Publish", "Esc Cancel"] {
        assert!(bar.contains(key), "{key}: {bar}");
    }
}

#[test]
fn a_refused_publish_whose_read_finds_a_publish_closes_the_form_and_says_why_on_the_message_line() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: crate::github::default_repo_name("Notes"),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    publish_reply(&mut shell, start, Err("github_publish_in_progress".into()));
    let status = IntakeQuery::GitHubPublishStatus { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status,
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Drafting,
        ))))),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(
        !shown.contains("[ Publish ]"),
        "a publish closes the form: {shown}"
    );
    assert_eq!(
        shell
            .ctx
            .message()
            .map(|message| (message.lead.clone(), message.rest.clone())),
        Some((
            super::new_project_publish::PUBLISH_NOT_STARTED.to_owned(),
            super::intake::copy("github_publish_in_progress"),
        )),
        "the reason the form showed is not lost with it"
    );
}

#[test]
fn sign_in_from_the_publish_question_closes_it_so_g_reads_the_sign_in_again() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    shell.arrive(owner_arrival(OwnerEvent::Intake(Box::new(IntakeEvent {
        query: IntakeQuery::GitHubStatus,
        result: Ok(IntakeView::GitHub(GitHubStatus {
            signed_in: false,
            login: None,
            method: GitHubMethod::None,
            waiting: false,
            problem: None,
        })),
    }))));
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::CloneRepo));
    shell.key(Key::Esc);
    assert!(
        matches!(shell.place(), Some(Place::Project { .. })),
        "{:?}",
        shell.place()
    );
    let shown = render(&mut shell, 100, 30);
    assert!(
        !shown.contains("Sign in to GitHub to publish this project."),
        "the question closed for the sign-in: {shown}"
    );
    assert!(
        shown.contains(super::project_publish::PROMPT_LINE),
        "{shown}"
    );
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert!(publish_asks(&mut shell).contains(&status));
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(true)));
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(
        publish_asks(&mut shell),
        [IntakeQuery::GitHubStatus],
        "Publish reads the sign-in again"
    );
}

/// Opens the notes project's Overview with one pool and no project pool, answers its publish
/// reads with no publish and the question offered, then presses Enter on Pick a pool.
/// Input: the width. Output: the shell with the pool picker open.
fn overview_picker(width: u16) -> Shell {
    let mut ctx = ctx(width, 30);
    ctx.store.pools = Load::Ready(vec![main_pool()]);
    ctx.store.pool_projects = Load::Ready(Vec::new());
    let project = ProjectId::new("notes").unwrap();
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(&mut shell, status, Ok(IntakeView::PublishStatus(None)));
    let prompt = IntakeQuery::GitHubPublishPrompt { project };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&prompt));
    publish_reply(&mut shell, prompt, Ok(IntakeView::Prompt(true)));
    shell.key(Key::Enter);
    shell
}

/// The Pick a pool box wraps its first sentence inside the box, so the owner reads it whole
/// at 80 and 100 columns; the 50-wide box cut it to "account…" before.
#[test]
fn the_pool_picker_wraps_its_sentence_inside_the_box() {
    for width in [80, 100] {
        let mut shell = overview_picker(width);
        let shown = render(&mut shell, width, 30);
        let lines: Vec<&str> = shown.lines().collect();
        let first = lines
            .iter()
            .position(|line| line.contains("Agents of this project use its pool's"))
            .unwrap_or_else(|| panic!("the picker is open at {width}: {shown}"));
        assert!(!lines[first].contains('…'), "{shown}");
        assert!(lines[first + 1].contains("accounts."), "{shown}");
        assert!(!lines[first + 1].contains('…'), "{shown}");
    }
}

#[test]
fn g_on_an_overview_with_its_pool_picker_open_leaves_the_publish_question_closed() {
    let mut shell = overview_picker(100);
    let picker = "Agents of this project use its pool's";
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains(picker), "{shown}");
    shell.key(Key::Char('G'));
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains(picker), "{shown}");
    assert!(
        !shown.contains("Don't ask again"),
        "G stays with the open picker: {shown}"
    );
}

#[test]
fn a_refused_publish_answer_reads_the_publish_again_and_shows_where_it_stands() {
    let mut ctx = ctx(100, 30);
    let project = ProjectId::new("notes").unwrap();
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes".into(),
            tab: ProjectTab::Overview,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status.clone(),
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Lines {
                lines: vec![".env*".into()],
                draft: GitHubIgnoreDraft::Agent,
            },
        ))))),
    );
    shell.key(Key::Enter);
    let approve = IntakeQuery::GitHubPublishApprove {
        project,
        lines: vec![".env*".into()],
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&approve));
    publish_reply(&mut shell, approve, Err("github_outcome_unknown".into()));
    let message = shell.ctx.message().expect("the refusal is said");
    assert_eq!(message.lead, "The publish answer wasn't taken.");
    assert_eq!(
        publish_asks(&mut shell),
        std::slice::from_ref(&status),
        "the publish is read again"
    );
    publish_reply(
        &mut shell,
        status,
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Done {
                full_name: "me/tool".into(),
                outcomes: Vec::<GitHubRuleOutcome>::new(),
            },
        ))))),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(shown.contains("Published as me/tool."), "{shown}");
    assert!(!shown.contains("[x] .env*"), "{shown}");
}

#[test]
fn a_start_whose_reply_was_lost_after_the_engine_ran_it_says_it_may_have_started() {
    let said = |result: Result<IntakeView, String>| {
        let message = super::project_page::ProjectPage::publish_start_said("Notes", &result);
        (message.lead, message.rest)
    };
    let maybe = (
        "Publishing on GitHub may have started.".to_owned(),
        "The project's Overview shows where it stands.".to_owned(),
    );
    assert_eq!(said(Err("github_outcome_unknown".into())), maybe);
    assert_eq!(said(Err("github_reply_bounds".into())), maybe);
    assert_eq!(said(Ok(IntakeView::Prompt(true))), maybe);
    assert_eq!(
        said(Err("github_publish_running".into())),
        (
            super::new_project_publish::PUBLISH_NOT_STARTED.to_owned(),
            super::intake::copy("github_publish_running"),
        )
    );
}

#[test]
fn a_start_whose_reply_was_lost_is_waited_for_as_long_as_the_engine_gives_a_start() {
    assert_eq!(
        crate::github::START_TIME,
        std::time::Duration::from_secs(140)
    );
    assert!(!super::project_publish::lost_start_over(
        1_000,
        1_000 + 139_999
    ));
    assert!(super::project_publish::lost_start_over(
        1_000,
        1_000 + 140_000
    ));
}

/// rv-code2 GSB-1: a Start whose reply was lost may still save its publish after the first
/// read, so the Overview reads again every `DRAFT_POLL_MS` with no question in between, and
/// shows the publish once it lands with no remount.
#[test]
fn a_start_whose_reply_was_lost_reads_the_publish_again_until_it_shows() {
    let (mut shell, project) = offered();
    shell.key(Key::Char('G'));
    shell.key(Key::Enter);
    assert_eq!(publish_asks(&mut shell), [IntakeQuery::GitHubStatus]);
    github_signed_in(&mut shell);
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let start = IntakeQuery::GitHubPublishStart {
        project: project.clone(),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: crate::github::default_repo_name("Notes"),
            description: crate::github::default_description(""),
            private: true,
            docs: false,
        },
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&start));
    publish_reply(&mut shell, start, Err("github_outcome_unknown".into()));
    let status = IntakeQuery::GitHubPublishStatus {
        project: project.clone(),
    };
    assert_eq!(publish_asks(&mut shell), std::slice::from_ref(&status));
    publish_reply(
        &mut shell,
        status.clone(),
        Ok(IntakeView::PublishStatus(None)),
    );
    assert!(
        publish_asks(&mut shell).is_empty(),
        "no question while the Start may still land"
    );
    std::thread::sleep(std::time::Duration::from_millis(
        super::project_publish::DRAFT_POLL_MS as u64 + 50,
    ));
    publish_reply(
        &mut shell,
        IntakeQuery::GitHubPublishStatus {
            project: ProjectId::new("other").unwrap(),
        },
        Ok(IntakeView::PublishStatus(None)),
    );
    assert_eq!(
        publish_asks(&mut shell),
        std::slice::from_ref(&status),
        "the publish is read again after DRAFT_POLL_MS"
    );
    publish_reply(
        &mut shell,
        status,
        Ok(IntakeView::PublishStatus(Some(Box::new(publish_job(
            GitHubPublishStep::Drafting,
        ))))),
    );
    let shown = render(&mut shell, 100, 30);
    assert!(
        shown.contains("Drafting the .gitignore lines..."),
        "{shown}"
    );
}

/// #177 F1: on the real project page's Settings tab, with its jobs loaded so the planning
/// keys are live, R reaches Restart them past the plugins section and the planning keys and
/// sends it for the project, and r still asks the roadmap status check and restarts nothing.
/// The engine's reads are answered here in the order the page makes them, Setup first.
#[test]
fn settings_with_jobs_loaded_sends_restart_them_on_capital_r_and_r_still_refreshes() {
    use crate::project_setup::{HiddenPaths, StoredSetup};
    use crate::team::{GraphNode, NodeKind, TeamAction, TeamGraph};
    use crate::tui::client::SetupEvent;
    use crate::tui::client::roadmap::{RoadmapEvent, RoadmapQuery};
    let project = ProjectId::new("notes-cli").unwrap();
    let stored = || StoredSetup {
        project_id: project.clone(),
        name: "Notes CLI".into(),
        about: "Terminal-first notes.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Cli],
        research: vec![ResearchKind::CompetitorResearch],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: "/private/tmp/ca-notes-cli".into(),
        created_ms: 1_793_000_000_000,
        revision: 4,
    };
    let seer = |id: &str, kind, label: &str| GraphNode {
        id: id.into(),
        kind,
        parent: None,
        label: label.into(),
        harness: "codex".into(),
        status: None,
        status_label: None,
        account: None,
        branch: None,
        terminal: None,
        questions: None,
        agent: None,
        pane: None,
        presence: None,
        waiting: None,
        waits_for: None,
        owner_questions: 0,
        no_worktree: false,
        hookless: false,
        message_note: None,
        started_ms: None,
        sees_hidden: true,
    };
    let graph = || TeamGraph {
        project_id: project.clone(),
        nodes: vec![
            seer("master", NodeKind::Master, "Master"),
            seer("team-1", NodeKind::Lead, "Ada"),
        ],
        edges: vec![],
        messages: Vec::new(),
        overlaps: Vec::new(),
        relations: Vec::new(),
        master_live_terminal: true,
    };
    let mut ctx = ctx(120, 40);
    ctx.link = crate::tui::ctx::Link::Ready;
    let screen = open(
        &Place::Project {
            id: project.clone(),
            name: "Notes CLI".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let mut read = (false, false, false);
    for _ in 0..64 {
        let requests: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
        if requests.is_empty() {
            break;
        }
        for request in requests {
            match request {
                Request::Setup(query) => {
                    let result = match &query {
                        SetupQuery::Setup { .. } => Ok(SetupView::Setup(Some(Box::new(stored())))),
                        SetupQuery::Jobs { .. } => {
                            read.0 = true;
                            Ok(SetupView::Jobs(Vec::new()))
                        }
                        SetupQuery::HiddenPaths { .. } => {
                            read.1 = true;
                            Ok(SetupView::HiddenPaths(HiddenPaths {
                                paths: vec!["/Users/owner/secret".into()],
                                version: 1,
                                runs: Vec::new(),
                                unavailable: false,
                            }))
                        }
                        _ => Err("row_skips_this_read".to_owned()),
                    };
                    shell.arrive(owner_arrival(OwnerEvent::Setup(Box::new(SetupEvent {
                        query,
                        result,
                    }))));
                }
                Request::TeamGraph { project } => {
                    read.2 = true;
                    shell.arrive(Arrival::TeamGraph {
                        project,
                        result: Ok(Box::new(graph())),
                    });
                }
                Request::Roadmap(query) => {
                    shell.arrive(owner_arrival(OwnerEvent::Roadmap(Box::new(RoadmapEvent {
                        query: *query,
                        result: Err("row_skips_this_read".to_owned()),
                    }))));
                }
                _ => {}
            }
        }
    }
    assert_eq!(
        read,
        (true, true, true),
        "the page read its jobs, its hidden folders and its team"
    );
    shell.key(Key::Char('R'));
    let restart: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(
        restart.iter().any(|request| matches!(
            request,
            Request::Setup(SetupQuery::TeamChange {
                action: TeamAction::RestartHiddenSeers { project_id },
                ..
            }) if *project_id == project
        )),
        "R restarts the agents that can still open a hidden folder: {restart:?}"
    );

    shell.key(Key::Char('r'));
    let refresh: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(
        refresh.iter().any(|request| matches!(
            request,
            Request::Roadmap(query) if **query == RoadmapQuery::Refresh { project: project.clone() }
        )),
        "r still asks the roadmap status check: {refresh:?}"
    );
    assert!(
        !refresh
            .iter()
            .any(|request| matches!(request, Request::Setup(SetupQuery::TeamChange { .. }))),
        "r restarts nothing: {refresh:?}"
    );
}
