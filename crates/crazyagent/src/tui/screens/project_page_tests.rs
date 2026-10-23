use super::new_project_page::{kept_draft, key_event, pool_choices_from};
use super::project_page::open_path;
use super::{Flow, Place, ProjectTab, open};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::pools::{MemberKind, MemberStatus, PoolMember, PoolSummary};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind, SetupQuery};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec};
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request, ShellEvent, ShellRequest};
use crate::tui::shell::Shell;
use crate::tui::theme::{Appearance, Colors, Depth};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
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

fn arrival(event: OwnerEvent) -> Arrival {
    match event {
        OwnerEvent::Access { ready: true, .. } => Arrival::Connected,
        OwnerEvent::Access { problem, .. } => Arrival::Disconnected(problem),
        OwnerEvent::Shell(event) => match *event {
            ShellEvent::Pools(result) => Arrival::Pools(result),
            ShellEvent::Projects(result) => Arrival::Projects(result),
            ShellEvent::Profile(result) => Arrival::Profile(result),
            ShellEvent::ProfileSaved(result) => Arrival::ProfileSaved(result),
            ShellEvent::PoolEdited(result) => Arrival::PoolEdited(result),
        },
        other => Arrival::Owner(Box::new(other)),
    }
}

async fn pump(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(arrival(event));
            moved = true;
        }
        if !connection.busy() && shell.ctx.link == crate::tui::ctx::Link::Ready {
            if let Some(request) = shell.ctx.next_request() {
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
                        });
                        connection.project_setup(query).unwrap();
                    }
                    other => panic!("project pages never send {other:?}"),
                }
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

struct Engine {
    root: tempfile::TempDir,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    controller: OwnerController,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-page-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let client = Client::new(root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
        let (_, controller) = OwnerController::attach_configured(client, OwnerClientKind::Tui)
            .await
            .unwrap();
        Self {
            root,
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
            Client::new(self.root.path().join("e.sock"), Duration::from_secs(2)).unwrap(),
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
fn opening_a_path_refuses_relative_and_missing_paths() {
    assert_eq!(
        open_path("context/brief.md"),
        Err("That file or folder is not there any more.".into())
    );
    assert_eq!(
        open_path("/private/tmp/ca-no-such-folder-4f1c"),
        Err("That file or folder is not there any more.".into())
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
    assert_eq!(sent, ["setup", "jobs", "context", "memories"]);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("CrazyAgent  /  Home  /  Notes CLI"),
        "{shown}"
    );
    assert!(shown.contains("Notes CLI keeps notes."), "{shown}");
    assert!(shown.contains("Not ready yet"), "{shown}");

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
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("The master conversation is not ready yet in this window."),
        "{shown}"
    );
    shell.key(Key::Char('h'));
    shell.key(Key::Enter);
    assert!(shell.ctx.next_request().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_opens_the_wizard_with_the_real_catalog_and_keeps_the_draft_on_create() {
    let engine = Engine::start().await;
    let mut connection = engine.connect();
    let mut ctx = ctx(80, 24);
    ctx.store.pools = Load::Ready(Vec::new());
    let screen = open(&Place::NewProject, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent, ["pools", "catalog"]);
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
    shell.key(Key::Enter);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("Creating projects is not ready yet."),
        "{shown}"
    );
    assert!(shell.ctx.next_request().is_none());
    shell.key(Key::Enter);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Tidepool"), "{shown}");
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
