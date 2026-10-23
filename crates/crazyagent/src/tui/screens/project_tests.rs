use super::new_project_view::{Look, Row};
use super::project::{
    PoolInfo, ProjectReply, ProjectScreen, Tab, memory_source, plain_problem, research_kind,
};
use super::project_markdown::line_width;
use super::project_view::project_screen;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::engine::{Client, EngineError};
use crate::memory::ReviewDecision;
use crate::project_setup::{
    ContextKind, Platform, ProjectSetup, ResearchKind, ResearchStatus, SetupAction, SetupQuery,
    SetupView,
};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shifted(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn text(rows: &[Row]) -> String {
    rows.iter().map(Row::text).collect::<Vec<_>>().join("\n")
}

fn changed(response: OwnerResponse) -> Result<SetupView, String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            Ok(SetupView::Changed(operation.result))
        }
        OwnerResponse::Rejected { code } => Err(code),
        _ => Err("setup_change_outcome_unknown".into()),
    }
}

async fn serve(controller: &mut OwnerController, query: SetupQuery) -> Result<SetupView, String> {
    let problem = |error: EngineError| error.0;
    Ok(match query {
        SetupQuery::Setup { project } => SetupView::Setup(
            controller
                .project_setup(project)
                .await
                .map_err(problem)?
                .map(Box::new),
        ),
        SetupQuery::Jobs { project } => {
            SetupView::Jobs(controller.research_jobs(project).await.map_err(problem)?)
        }
        SetupQuery::Context { project } => {
            SetupView::Context(controller.project_context(project).await.map_err(problem)?)
        }
        SetupQuery::Item {
            project,
            path,
            offset,
        } => SetupView::Item(
            controller
                .project_context_item(project, path, offset)
                .await
                .map_err(problem)?,
        ),
        SetupQuery::Memories { project, after } => SetupView::Memories(
            controller
                .project_memories(project, after)
                .await
                .map_err(problem)?,
        ),
        SetupQuery::ReviewMemory {
            project,
            entry_id,
            version,
            decision,
            generation,
        } => changed(
            controller
                .review_memory(project, entry_id, version, decision, generation)
                .await
                .map_err(problem)?,
        )?,
        SetupQuery::Change { action, generation } => changed(
            match action {
                SetupAction::SaveToMemory {
                    project_id,
                    research,
                    section,
                } => {
                    controller
                        .save_research_to_memory(project_id, research, section, generation)
                        .await
                }
                SetupAction::DeleteContext { project_id, path } => {
                    controller
                        .delete_project_context(project_id, path, generation)
                        .await
                }
                other => panic!("the project screen never sends {other:?}"),
            }
            .map_err(problem)?,
        )?,
        SetupQuery::Catalog { .. } => panic!("the project screen never reads the catalog"),
    })
}

async fn settle(
    screen: &mut ProjectScreen,
    controller: &mut OwnerController,
    now_ms: i64,
) -> Vec<&'static str> {
    let mut sent = Vec::new();
    for _ in 0..12 {
        let Some(query) = screen.next_query(now_ms) else {
            return sent;
        };
        assert!(screen.next_query(now_ms).is_none(), "one read at a time");
        sent.push(match &query {
            SetupQuery::Setup { .. } => "setup",
            SetupQuery::Jobs { .. } => "jobs",
            SetupQuery::Context { .. } => "context",
            SetupQuery::Item { .. } => "item",
            SetupQuery::Memories { .. } => "memories",
            _ => "other",
        });
        let result = serve(controller, query.clone()).await;
        screen.apply(&query, result);
    }
    panic!("reads never settled: {sent:?}");
}

async fn send(screen: &mut ProjectScreen, controller: &mut OwnerController, query: SetupQuery) {
    let result = serve(controller, query.clone()).await;
    assert!(result.is_ok(), "engine refused {query:?}: {result:?}");
    screen.apply(&query, result);
}

#[tokio::test]
async fn project_screen_reads_a_real_project_and_saves_its_research_to_memory() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-project-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("scope.csv"), "item,size\nlogin,small\n").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let project = ProjectId::new("notes-cli").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Notes CLI".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Terminal-first notes.\nSecond line.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Cli],
                research: vec![
                    ResearchKind::CompetitorResearch,
                    ResearchKind::TechStackResearch,
                ],
                pool: None,
                team: None,
                documents: vec![sources.path().join("scope.csv").to_str().unwrap().into()],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));

    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let start = 1_793_000_000_000;
    assert_eq!(
        settle(&mut screen, &mut controller, start).await,
        ["setup", "jobs", "context", "memories"]
    );
    assert_eq!(screen.name, "Notes CLI");
    assert_eq!(screen.problem, None);
    let jobs = screen.jobs.as_ref().unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind, job.status))
            .collect::<Vec<_>>(),
        [
            (ResearchKind::CompetitorResearch, ResearchStatus::NeedsPool),
            (ResearchKind::TechStackResearch, ResearchStatus::NeedsPool)
        ]
    );
    assert_eq!(screen.counts(), (0, 2, 0, 0));
    assert!(screen.suggested().is_empty());
    assert_eq!(
        screen
            .tree()
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        ["context/brief.md", "context/documents/scope.csv"]
    );
    assert!(
        settle(&mut screen, &mut controller, start + 1_000)
            .await
            .is_empty()
    );
    assert_eq!(
        settle(&mut screen, &mut controller, start + 3_000).await,
        ["jobs", "memories"]
    );

    let look = Look::plain();
    for (width, height) in [(120u16, 36u16), (80, 24)] {
        let drawn = project_screen(&mut screen, width, height, &look);
        assert_eq!(drawn.rows.len(), usize::from(height) - 4);
        assert!(
            drawn
                .rows
                .iter()
                .all(|row| row.width() == usize::from(width))
        );
        assert_eq!(drawn.crumbs, ["Home", "Notes CLI"]);
        let shown = text(&drawn.rows);
        assert!(
            shown.contains("This project has no pool, so no agent can start."),
            "{shown}"
        );
        assert!(shown.contains("Terminal-first notes."));
        assert!(!shown.contains("Second line."));
        assert!(shown.contains("Competitor research"));
        assert!(shown.contains("! Needs a pool"));
        assert_eq!(drawn.help.first(), Some(&("Enter", "Pick a pool")));
    }
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::PickPool);
    let picked = CredentialRef::new("pool-main").unwrap();
    screen.setup.as_mut().unwrap().pool = Some(picked.clone());
    screen.pool = Some(PoolInfo {
        id: picked.clone(),
        name: "Main".into(),
        accounts: 1,
        assigned: false,
    });
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(
        shown.contains("Pool Main is picked but not assigned yet"),
        "{shown}"
    );
    assert!(shown.contains("[> Assign Main ]"));
    assert_eq!(drawn.help.first(), Some(&("Enter", "Assign pool")));
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Assign(picked.clone())
    );
    screen.open(Tab::Settings);
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Assign(picked)
    );
    screen.pool.as_mut().unwrap().assigned = true;
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::PickPool);
    screen.open(Tab::Overview);
    screen.setup.as_mut().unwrap().pool = None;
    screen.pool = None;

    let folder = screen.setup.as_ref().unwrap().folder.clone();
    let report = std::path::Path::new(&folder).join(ResearchKind::CompetitorResearch.report_path());
    std::fs::create_dir_all(report.parent().unwrap()).unwrap();
    let long = "word ".repeat(400);
    std::fs::write(
        &report,
        format!("# Competitors\n\nBuoyWatch charges per seat. \u{1b}[31mred\u{1b}[0m\n\n{long}\n"),
    )
    .unwrap();

    screen.open(Tab::Context);
    screen.width = 120;
    settle(&mut screen, &mut controller, start + 6_000).await;
    let tree: Vec<(String, ContextKind)> = screen
        .tree()
        .iter()
        .map(|item| (item.path.clone(), item.kind))
        .collect();
    let research = ResearchKind::CompetitorResearch.report_path();
    assert_eq!(
        tree,
        [
            ("context/brief.md".to_owned(), ContextKind::Brief),
            (research.clone(), ContextKind::Research),
            (
                "context/documents/scope.csv".to_owned(),
                ContextKind::Document
            ),
        ]
    );
    assert_eq!(screen.key(key(KeyCode::Char('d'))), ProjectReply::Handled);
    assert_eq!(screen.confirm_delete, None);
    assert_eq!(
        screen.problem.as_deref(),
        Some("The project brief can't be deleted.")
    );
    assert_eq!(screen.key(key(KeyCode::Down)), ProjectReply::Handled);
    assert_eq!(screen.problem, None);
    assert_eq!(
        screen.preview.as_ref().map(|p| p.path.as_str()),
        Some(research.as_str())
    );
    assert_eq!(
        settle(&mut screen, &mut controller, start + 6_000).await,
        ["item"]
    );
    let preview = screen.preview.as_ref().unwrap();
    assert!(preview.text.contains("BuoyWatch charges per seat."));
    assert_eq!(preview.next, None);

    let drawn = project_screen(&mut screen, 120, 36, &look);
    let shown = text(&drawn.rows);
    assert!(drawn.rows.iter().all(|row| row.width() == 120));
    assert!(shown.contains("brief.md"));
    assert!(shown.contains("research/"));
    assert!(shown.contains("documents/"));
    assert!(shown.contains("scope.csv"));
    assert!(shown.contains("BuoyWatch charges per seat. red"), "{shown}");
    assert!(!shown.contains('\u{1b}'));
    assert!(!shown.contains("# Competitors"));
    assert!(shown.contains("Competitors"));
    for line in screen.preview.as_mut().unwrap().lines(77) {
        assert!(line_width(line) <= 77);
    }

    screen.width = 80;
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert!(screen.reading);
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(
        drawn.crumbs.last().map(String::as_str),
        research.rsplit('/').next()
    );
    assert!(drawn.rows.iter().all(|row| row.width() == 80));
    assert_eq!(screen.key(key(KeyCode::PageDown)), ProjectReply::Handled);
    assert_eq!(screen.preview.as_ref().unwrap().scroll, 10);
    project_screen(&mut screen, 80, 24, &look);
    assert!(screen.preview.as_ref().unwrap().scroll <= 10);

    let save = screen.key(key(KeyCode::Char('m')));
    let ProjectReply::Change(action) = save else {
        panic!("Save to memory must ask the engine, got {save:?}");
    };
    assert_eq!(
        *action,
        SetupAction::SaveToMemory {
            project_id: project.clone(),
            research: ResearchKind::CompetitorResearch,
            section: None
        }
    );
    send(
        &mut screen,
        &mut controller,
        SetupQuery::Change {
            action: *action,
            generation: 2,
        },
    )
    .await;
    assert_eq!(screen.problem, None);
    screen.open(Tab::Overview);
    settle(&mut screen, &mut controller, start + 9_000).await;
    assert_eq!(screen.suggested().len(), 1);
    assert_eq!(
        memory_source(screen.suggested()[0]),
        Some(ResearchKind::CompetitorResearch)
    );
    let drawn = project_screen(&mut screen, 120, 36, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("Needs you  1"), "{shown}");
    assert!(shown.contains("Save to memory?"));
    assert!(
        shown.contains("\"BuoyWatch charges per seat. red\""),
        "{shown}"
    );
    assert!(shown.contains("From Competitor research"), "{shown}");
    assert!(drawn.help.contains(&("y", "Save to memory")));

    screen.width = 80;
    screen.read(&research);
    assert!(screen.report_suggestion().is_some());
    screen.preview.as_mut().unwrap().scroll = usize::MAX / 2;
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("Suggested memory"), "{shown}");
    assert!(shown.contains("y Save to memory   n Dismiss   e Edit first"));
    assert_eq!(drawn.help.first(), Some(&("y", "Save to memory")));
    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(screen.edit.is_some());
    screen.edit = None;
    screen.preview = None;
    screen.reading = false;
    screen.open(Tab::Overview);
    screen.width = 120;

    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(screen.edit.is_some());
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(text(&drawn.rows).contains("Save to memory"));
    assert!(drawn.cursor.is_some());
    assert_eq!(screen.paste(" Seen."), ProjectReply::Handled);
    let review = screen.key(key(KeyCode::Enter));
    let ProjectReply::Review {
        entry,
        version,
        decision,
    } = review
    else {
        panic!("Enter in the edit box must save, got {review:?}");
    };
    let ReviewDecision::Edit { text: edited } = &decision else {
        panic!("an edited memory must be saved as an edit, got {decision:?}");
    };
    assert!(edited.starts_with("# Competitors"));
    assert!(edited.ends_with("word Seen."), "{edited}");
    assert!(!edited.contains('\u{1b}'));
    assert!(screen.edit.is_none());
    send(
        &mut screen,
        &mut controller,
        SetupQuery::ReviewMemory {
            project: project.clone(),
            entry_id: entry,
            version,
            decision,
            generation: 3,
        },
    )
    .await;
    settle(&mut screen, &mut controller, start + 12_000).await;
    assert!(screen.suggested().is_empty());

    screen.open(Tab::Context);
    settle(&mut screen, &mut controller, start + 15_000).await;
    screen.tree_cursor = 2;
    assert_eq!(screen.key(key(KeyCode::Char('d'))), ProjectReply::Handled);
    assert_eq!(
        screen.confirm_delete.as_deref(),
        Some("context/documents/scope.csv")
    );
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(text(&drawn.rows).contains("Delete scope.csv?"));
    assert_eq!(screen.key(key(KeyCode::Char('n'))), ProjectReply::Handled);
    assert_eq!(screen.confirm_delete, None);
    screen.key(key(KeyCode::Char('d')));
    let delete = screen.key(key(KeyCode::Char('y')));
    let ProjectReply::Change(action) = delete else {
        panic!("y must delete, got {delete:?}");
    };
    send(
        &mut screen,
        &mut controller,
        SetupQuery::Change {
            action: *action,
            generation: 4,
        },
    )
    .await;
    settle(&mut screen, &mut controller, start + 18_000).await;
    assert!(
        !screen
            .tree()
            .iter()
            .any(|item| item.path == "context/documents/scope.csv")
    );
    assert!(sources.path().join("scope.csv").exists());
}

#[test]
fn tabs_input_and_escape_follow_the_design_keys() {
    let mut screen = ProjectScreen::new(ProjectId::new("keys").unwrap(), "Keys".into(), None);
    assert_eq!(screen.tab, Tab::Overview);
    assert_eq!(screen.key(key(KeyCode::Char(']'))), ProjectReply::Handled);
    assert_eq!(screen.tab, Tab::Conversation);
    assert!(screen.typing);
    assert_eq!(screen.key(key(KeyCode::Char('['))), ProjectReply::Handled);
    assert_eq!(screen.tab, Tab::Conversation);
    assert_eq!(screen.input.value(), "[");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "[", "no pool means nothing is sent");
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(!screen.typing);
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert!(screen.typing);
    assert_eq!(screen.key(shifted(KeyCode::Right)), ProjectReply::Handled);
    assert_eq!(screen.tab, Tab::Agents);
    assert!(!screen.typing);
    assert_eq!(screen.key(key(KeyCode::Down)), ProjectReply::Handled);
    assert_eq!(screen.agents_cursor, 0);
    assert_eq!(screen.key(shifted(KeyCode::Left)), ProjectReply::Handled);
    assert_eq!(screen.key(shifted(KeyCode::Left)), ProjectReply::Handled);
    assert_eq!(screen.key(shifted(KeyCode::Left)), ProjectReply::Handled);
    assert_eq!(screen.tab, Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::PickPool);
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Home);
    screen.open(Tab::Context);
    assert_eq!(screen.key(key(KeyCode::Char('d'))), ProjectReply::Handled);
    assert_eq!(screen.confirm_delete, None);
    assert_eq!(screen.key(key(KeyCode::Char('q'))), ProjectReply::Ignored);
}

#[test]
fn a_new_screen_reads_its_setup_first_and_waits_for_each_reply() {
    let project = ProjectId::new("order").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Order".into(), None);
    let first = screen.next_query(10).unwrap();
    assert_eq!(
        first,
        SetupQuery::Setup {
            project: project.clone()
        }
    );
    assert_eq!(screen.next_query(10), None);
    screen.apply(&first, Err("trusted_connection_unavailable".into()));
    assert_eq!(
        screen.problem.as_deref(),
        Some("Not connected to the engine. Retrying.")
    );
    assert_eq!(
        screen.next_query(20),
        Some(SetupQuery::Setup {
            project: project.clone()
        })
    );
    screen.apply(
        &SetupQuery::Setup {
            project: project.clone(),
        },
        Ok(SetupView::Setup(None)),
    );
    assert!(screen.setup_missing);
    assert_eq!(screen.next_query(30), None);
    let drawn = project_screen(&mut screen, 80, 24, &Look::plain());
    assert!(text(&drawn.rows).contains("Its master conversation still works in Conversation."));
}

#[test]
fn report_paths_map_back_to_their_research() {
    for kind in [
        ResearchKind::MarketResearch,
        ResearchKind::CompetitorResearch,
        ResearchKind::TechStackResearch,
    ] {
        assert_eq!(research_kind(&kind.report_path()), Some(kind));
    }
    assert_eq!(research_kind("context/documents/market_research.md"), None);
    assert_eq!(research_kind("context/research/unknown.md"), None);
    assert_eq!(research_kind("research/market_research.md"), None);
    assert_eq!(
        plain_problem("memory_entry_exists"),
        "This report is already saved to memory."
    );
    assert_eq!(
        plain_problem("anything_else"),
        "That didn't work. Try again."
    );
}

#[test]
fn a_memory_stands_for_itself_by_its_first_line_of_prose() {
    use super::project_view::memory_quote;
    assert_eq!(
        memory_quote("# Competitors\n\nBuoyWatch charges per seat.\nMore."),
        "BuoyWatch charges per seat."
    );
    assert_eq!(memory_quote("## Only a heading\n"), "Only a heading");
    assert_eq!(memory_quote("  \n"), "");
}
