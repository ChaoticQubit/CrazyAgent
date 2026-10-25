use super::new_project_frames::{check_frames, compose};
use super::new_project_view::Look;
use super::project::{ProjectScreen, Tab};
use super::project_view::project_screen;
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind, SetupQuery, SetupView};
use crate::protocol::{ProjectId, ProjectSpec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;

const REPORT: &str = "# Market research: Fieldnotes Mobile

Ecologists keep field notes on paper or in general note apps that lose data without signal. Most want one offline notebook that syncs later.

## Who pays
- University ecology labs, mostly grant funded
- Park and reserve services
- Environmental consultancies

## Sources
1. National Park Service data  https://www.nps.gov
";

fn clocks(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < characters.len() {
        let window = characters.get(index..index + 5);
        let clock = window.is_some_and(|w| {
            w[0].is_ascii_digit()
                && w[1].is_ascii_digit()
                && w[2] == ':'
                && w[3].is_ascii_digit()
                && w[4].is_ascii_digit()
        });
        if clock {
            out.push_str("hh:mm");
            index += 5;
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    out
}

async fn read(screen: &mut ProjectScreen, controller: &mut OwnerController, now_ms: i64) {
    for _ in 0..12 {
        let Some(query) = screen.next_query(now_ms) else {
            return;
        };
        let result = match query.clone() {
            SetupQuery::Setup { project } => controller
                .project_setup(project)
                .await
                .map(|setup| SetupView::Setup(setup.map(Box::new))),
            SetupQuery::Jobs { project } => {
                controller.research_jobs(project).await.map(SetupView::Jobs)
            }
            SetupQuery::Context { project } => controller
                .project_context(project)
                .await
                .map(SetupView::Context),
            SetupQuery::Item {
                project,
                path,
                offset,
            } => controller
                .project_context_item(project, path, offset)
                .await
                .map(SetupView::Item),
            SetupQuery::Memories { project, after } => controller
                .project_memories(project, after)
                .await
                .map(SetupView::Memories),
            other => panic!("unexpected read {other:?}"),
        };
        screen.apply(&query, result.map_err(|error| error.0));
    }
    panic!("reads never settled");
}

#[tokio::test]
async fn project_frames_from_a_real_engine_match_the_design() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-frames-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let owner = tempfile::Builder::new()
        .prefix("ca-tui-frames-owner-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(owner.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner_home = std::fs::canonicalize(owner.path()).unwrap();
    let projects = owner_home.join("projects");
    std::fs::create_dir(&projects).unwrap();
    let homed = controller
        .set_projects_home(projects.to_str().unwrap().into(), 1)
        .await
        .unwrap();
    assert!(
        matches!(
            &homed,
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ),
        "{homed:?}"
    );
    let project = ProjectId::new("fieldnotes-mobile").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Fieldnotes Mobile".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Offline field notes for ecologists.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::MobileApp],
                research: vec![
                    ResearchKind::MarketResearch,
                    ResearchKind::DesignResearch,
                    ResearchKind::FrontEndDesign,
                ],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));
    let mut screen = ProjectScreen::new(project, String::new(), Some(owner_home));
    let start = 1_793_000_000_000;
    read(&mut screen, &mut controller, start).await;
    let state = screen.setup.as_ref().unwrap().state_folder();
    let report = state.join(ResearchKind::MarketResearch.report_path());
    std::fs::create_dir_all(report.parent().unwrap()).unwrap();
    std::fs::write(&report, REPORT).unwrap();

    let look = Look::plain();
    let mut drawn = Vec::new();
    let mut draw = |id: &str, screen: &mut ProjectScreen, width: u16, height: u16| {
        screen.width = width;
        let frame = project_screen(screen, width, height, &look);
        drawn.push((
            id.to_owned(),
            clocks(&compose(&frame, width, height, "", &look)),
        ));
    };
    draw("proj-overview-nopool-120", &mut screen, 120, 36);
    draw("proj-overview-nopool-80", &mut screen, 80, 24);
    screen.open(Tab::Conversation);
    draw("proj-conv-nopool-80", &mut screen, 80, 24);
    screen.open(Tab::Agents);
    draw("proj-agents-120", &mut screen, 120, 36);
    draw("proj-agents-80", &mut screen, 80, 24);
    screen.open(Tab::Settings);
    draw("proj-settings-120", &mut screen, 120, 36);
    draw("proj-settings-80", &mut screen, 80, 24);
    screen.open(Tab::Context);
    screen.context = None;
    read(&mut screen, &mut controller, start + 3_000).await;
    screen.width = 80;
    screen.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    draw("proj-context-80", &mut screen, 80, 24);
    screen.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    read(&mut screen, &mut controller, start + 3_000).await;
    draw("proj-context-preview-80", &mut screen, 80, 24);
    screen.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    draw("proj-context-120", &mut screen, 120, 36);
    check_frames(&drawn);
}
