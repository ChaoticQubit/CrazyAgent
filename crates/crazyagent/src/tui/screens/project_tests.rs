use super::new_project_view::{Look, Row};
use super::project::{
    JobLog, MasterStatus, PoolInfo, ProjectReply, ProjectScreen, REFRESH_MS, Tab, Transcript,
    memory_source, plain_problem, research_kind,
};
use super::project_markdown::line_width;
use super::project_view::{activity, job_status, project_screen};
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crate::engine::{Client, EngineError};
use crate::memory::ReviewDecision;
use crate::project_setup::{
    ContextKind, Platform, ProjectSetup, ResearchJob, ResearchKind, ResearchStatus, SetupAction,
    SetupQuery, SetupView, StoredSetup, TeamPlan,
};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;

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
        SetupQuery::Log { project, job } => SetupView::Log(
            controller
                .research_log(project, job)
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
                SetupAction::CancelResearch { project_id, job } => {
                    controller
                        .cancel_research(project_id, job, generation)
                        .await
                }
                SetupAction::RetryResearch { project_id, job } => {
                    controller.retry_research(project_id, job, generation).await
                }
                other => panic!("the project screen never sends {other:?}"),
            }
            .map_err(problem)?,
        )?,
        SetupQuery::Catalog { .. } => panic!("the project screen never reads the catalog"),
        SetupQuery::Team { .. } | SetupQuery::TeamChange { .. } | SetupQuery::Library(_) => {
            panic!("the project screen never reads the team or the library")
        }
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
            SetupQuery::Log { .. } => "log",
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
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("scope.csv"), "item,size\nlogin,small\n").unwrap();
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
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
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
        ProjectReply::Assign(Some(picked.clone()))
    );
    screen.open(Tab::Settings);
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(drawn.help.first(), Some(&("Enter", "Assign pool")));
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Assign(Some(picked))
    );
    screen.pool.as_mut().unwrap().assigned = true;
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(drawn.help.first(), Some(&("Enter", "Change pool")));
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::PickPool);
    let assigned = screen.pool.take();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(drawn.help.first(), Some(&("Enter", "Pick a pool")));
    screen.pool = assigned;
    screen.open(Tab::Overview);
    screen.setup.as_mut().unwrap().pool = None;
    screen.pool = None;

    let state = screen.setup.as_ref().unwrap().state_folder();
    let report = state.join(ResearchKind::CompetitorResearch.report_path());
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
    assert_eq!(
        screen.problem.as_deref(),
        Some(
            "This part has hidden or direction-changing characters, so it can't be saved to memory."
        )
    );
    std::fs::write(
        &report,
        format!("# Competitors\n\nBuoyWatch charges per seat. red\n\n{long}\n"),
    )
    .unwrap();
    let retry = screen.key(key(KeyCode::Char('m')));
    assert_eq!(screen.problem, None);
    let ProjectReply::Change(action) = retry else {
        panic!("Save to memory must ask the engine again, got {retry:?}");
    };
    send(
        &mut screen,
        &mut controller,
        SetupQuery::Change {
            action: *action,
            generation: 3,
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
    assert_eq!(
        screen.key(key(KeyCode::Char('y'))),
        ProjectReply::Handled,
        "y must show the whole memory before it saves"
    );
    let drawn = project_screen(&mut screen, 120, 36, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("# Competitors"), "{shown}");
    assert!(!drawn.help.contains(&("y", "Save to memory")));
    assert_eq!(screen.key(key(KeyCode::Char('y'))), ProjectReply::Handled);
    for _ in 0..40 {
        assert_eq!(screen.key(key(KeyCode::PageDown)), ProjectReply::Handled);
    }
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(text(&drawn.rows).contains("word word"));
    assert!(drawn.help.contains(&("y", "Save to memory")));
    let kept = screen.key(key(KeyCode::Char('y')));
    assert!(
        matches!(
            kept,
            ProjectReply::Review {
                decision: ReviewDecision::Keep,
                ..
            }
        ),
        "{kept:?}"
    );
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(!text(&drawn.rows).contains("# Competitors"));
    assert_eq!(screen.key(key(KeyCode::Char('y'))), ProjectReply::Handled);
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(!text(&drawn.rows).contains("# Competitors"));

    screen.width = 80;
    screen.read(&research);
    assert!(screen.report_suggestion().is_some());
    screen.preview.as_mut().unwrap().scroll = usize::MAX / 2;
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("Suggested memory"), "{shown}");
    assert!(shown.contains("y Save to memory   n Dismiss   e Edit first"));
    assert_eq!(drawn.help.first(), Some(&("y", "Save to memory")));
    assert_eq!(screen.key(key(KeyCode::Char('y'))), ProjectReply::Handled);
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("# Competitors"));
    assert!(drawn.rows.iter().all(|row| row.width() == 80));
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.reading);
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
            generation: 4,
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
            generation: 5,
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
    assert_eq!(screen.next_query(20), None);
    assert_eq!(
        screen.next_query(10 + REFRESH_MS),
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
    assert_eq!(screen.next_query(20 + REFRESH_MS), None);
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
        "That didn't work. Try again. (code: anything_else)"
    );
    assert_eq!(
        plain_problem("disk I/O error at /Users/owner/notes"),
        "That didn't work. Try again. (code: engine_error_unplain)"
    );
    assert_eq!(
        plain_problem("project_folder_unwritable"),
        "CrazyAgent can't write in the project folder. Check its permissions."
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

#[test]
fn the_agent_activity_lists_only_what_the_engine_recorded() {
    let look = Look::plain();
    let mut job = ResearchJob {
        id: 7,
        project_id: ProjectId::new("notes-cli").unwrap(),
        kind: ResearchKind::MarketResearch,
        kind_label: "Market research".into(),
        status: ResearchStatus::NeedsPool,
        status_label: "Needs a pool".into(),
        report_path: "context/research/market_research.md".into(),
        harness: "claude".into(),
        tools: true,
        attempts: 0,
        claimer: None,
        error: None,
        cancel_requested: false,
        output_bytes: 0,
        created_ms: 1_000,
        started_ms: None,
        heartbeat_ms: None,
        finished_ms: None,
    };
    let steps = |job: &ResearchJob| -> Vec<(Option<i64>, String)> {
        activity(job, &look)
            .into_iter()
            .map(|(at, text, _)| (at, text))
            .collect()
    };
    assert_eq!(
        steps(&job),
        [
            (Some(1_000), "Queued".to_owned()),
            (None, "Waiting for a pool".to_owned())
        ]
    );
    job.status = ResearchStatus::Running;
    job.attempts = 2;
    job.started_ms = Some(2_000);
    job.heartbeat_ms = Some(3_000);
    job.output_bytes = 12 * 1024;
    job.cancel_requested = true;
    assert_eq!(
        steps(&job),
        [
            (Some(1_000), "Queued".to_owned()),
            (Some(2_000), "Started on Claude Code, try 2".to_owned()),
            (
                Some(3_000),
                "Still working, 12 KB of output so far".to_owned()
            ),
            (None, "Stop asked, waiting for the agent to end".to_owned()),
        ]
    );
    job.status = ResearchStatus::Done;
    job.attempts = 1;
    job.cancel_requested = false;
    job.finished_ms = Some(4_000);
    assert_eq!(
        steps(&job),
        [
            (Some(1_000), "Queued".to_owned()),
            (Some(2_000), "Started on Claude Code".to_owned()),
            (Some(4_000), "Finished, report written".to_owned()),
        ]
    );
    job.status = ResearchStatus::Failed;
    assert_eq!(steps(&job).last().unwrap().1, "Failed");
    job.status = ResearchStatus::Cancelled;
    assert_eq!(steps(&job).last().unwrap().1, "Stopped");
}

#[tokio::test]
async fn agents_stop_after_a_yes_run_again_and_show_the_agents_latest_output() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-agents-")
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
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let project = ProjectId::new("buoy-watch").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Buoy Watch".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Alerts when buoy readings look wrong.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Cli],
                research: vec![
                    ResearchKind::MarketResearch,
                    ResearchKind::CompetitorResearch,
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

    let mut screen = ProjectScreen::new(project.clone(), String::new(), None);
    let start = 1_793_000_000_000;
    assert_eq!(
        settle(&mut screen, &mut controller, start).await,
        ["setup", "jobs", "context", "memories"],
        "no agent output is read outside the Agents tab"
    );
    screen.open(Tab::Agents);
    assert_eq!(
        settle(&mut screen, &mut controller, start + 1_000).await,
        ["log"]
    );
    let first = screen.focused_job().unwrap().clone();
    assert_eq!(first.status, ResearchStatus::NeedsPool);
    assert_eq!(
        screen.log.as_ref().map(|log| (log.job, log.lines.clone())),
        Some((first.id, Some(Vec::new())))
    );
    assert!(
        settle(&mut screen, &mut controller, start + 2_000)
            .await
            .is_empty(),
        "a waiting agent's output is not read again until its status changes"
    );
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(drawn.help[1], ("x", "Stop"));
    assert!(!text(&drawn.rows).contains("No output yet."));

    assert_eq!(screen.key(key(KeyCode::Char('r'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("Only failed or stopped research can run again.")
    );
    assert_eq!(screen.key(key(KeyCode::Char('x'))), ProjectReply::Handled);
    assert_eq!(screen.confirm_stop, Some(first.id));
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("Stop Market research?"));
    assert_eq!(drawn.help[0], ("y", "Stop"));
    assert_eq!(screen.key(key(KeyCode::Char('n'))), ProjectReply::Handled);
    assert_eq!(screen.confirm_stop, None);
    screen.key(key(KeyCode::Char('x')));
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert_eq!(
        screen.confirm_stop,
        Some(first.id),
        "only y, n or Esc answer"
    );
    let stop = screen.key(key(KeyCode::Char('y')));
    assert_eq!(
        stop,
        ProjectReply::Change(Box::new(SetupAction::CancelResearch {
            project_id: project.clone(),
            job: first.id,
        }))
    );
    assert_eq!(screen.confirm_stop, None);
    let ProjectReply::Change(action) = stop else {
        unreachable!()
    };
    send(
        &mut screen,
        &mut controller,
        SetupQuery::Change {
            action: *action,
            generation: 2,
        },
    )
    .await;

    let run = root
        .path()
        .join("research/agents")
        .join(format!("job-{}-1", first.id));
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        run.join("stderr.log"),
        "Searching ocean sensor pricing\nRead buoywatch.io/pricing\nwarning:\tslow network\n",
    )
    .unwrap();
    assert_eq!(
        settle(&mut screen, &mut controller, start + 3_000).await,
        ["jobs", "log"]
    );
    assert_eq!(
        screen.focused_job().map(|job| job.status),
        Some(ResearchStatus::Cancelled)
    );
    assert_eq!(
        screen.log.as_ref().and_then(|log| log.lines.clone()),
        Some(vec![
            "Searching ocean sensor pricing".to_owned(),
            "Read buoywatch.io/pricing".to_owned(),
            "warning: slow network".to_owned(),
        ])
    );
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("Stopped"), "{shown}");
    assert!(
        shown.contains("       Read buoywatch.io/pricing"),
        "{shown}"
    );
    assert!(shown.contains("warning: slow network"), "{shown}");
    assert_eq!(drawn.help[1], ("r", "Retry"));
    assert_eq!(screen.key(key(KeyCode::Char('x'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("This research is not running.")
    );
    assert_eq!(screen.confirm_stop, None);

    let again = screen.key(key(KeyCode::Char('r')));
    assert_eq!(
        again,
        ProjectReply::Change(Box::new(SetupAction::RetryResearch {
            project_id: project.clone(),
            job: first.id,
        }))
    );
    let ProjectReply::Change(action) = again else {
        unreachable!()
    };
    send(
        &mut screen,
        &mut controller,
        SetupQuery::Change {
            action: *action,
            generation: 3,
        },
    )
    .await;
    assert_eq!(
        settle(&mut screen, &mut controller, start + 4_000).await,
        ["jobs", "log"]
    );
    assert_eq!(
        screen.focused_job().map(|job| (job.status, job.attempts)),
        Some((ResearchStatus::NeedsPool, 0)),
        "with no pool the retried research waits for one"
    );

    let stale = SetupQuery::Change {
        action: SetupAction::RetryResearch {
            project_id: project.clone(),
            job: first.id,
        },
        generation: 4,
    };
    let refused = serve(&mut controller, stale.clone()).await;
    assert_eq!(
        refused,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "research_job_not_retryable".into()
        }))
    );
    screen.apply(&stale, refused);
    assert_eq!(
        screen.problem.as_deref(),
        Some("Only failed or stopped research can run again.")
    );
    assert_eq!(
        settle(&mut screen, &mut controller, start + 4_500).await,
        ["jobs"],
        "a refused change reads the jobs again"
    );
}

#[test]
fn a_job_waiting_on_a_rate_limit_reads_rate_limited_until_its_local_end() {
    const REASON: &str =
        "Every account in this project's pool that can run this research hit its rate limit";
    let look = Look::plain();
    let limited = ResearchJob {
        id: 9,
        project_id: ProjectId::new("notes-cli").unwrap(),
        kind: ResearchKind::CompetitorResearch,
        kind_label: "Competitor research".into(),
        status: ResearchStatus::NeedsPool,
        status_label: "Rate limited".into(),
        report_path: "context/research/competitor_research.md".into(),
        harness: "claude".into(),
        tools: false,
        attempts: 0,
        claimer: None,
        error: Some(format!("rate_limited until 2026-10-23 04:32 UTC: {REASON}")),
        cancel_requested: false,
        output_bytes: 0,
        created_ms: 1_000,
        started_ms: None,
        heartbeat_ms: None,
        finished_ms: None,
    };
    assert_eq!(
        text(&[job_status(&limited, 0, 3_600, &look)]),
        "! Rate limited until 05:32"
    );
    assert_eq!(
        activity(&limited, &look)
            .into_iter()
            .map(|(at, text, _)| (at, text))
            .collect::<Vec<_>>(),
        [
            (Some(1_000), "Queued".to_owned()),
            (None, REASON.to_owned())
        ]
    );
    assert_eq!(
        crate::tui::seam::ResearchCounts::of(std::slice::from_ref(&limited)).summary(),
        "Research: rate limited"
    );
    let mut waiting = limited.clone();
    waiting.error = None;
    assert_eq!(
        text(&[job_status(&waiting, 0, 3_600, &look)]),
        "! Needs a pool"
    );
    waiting.error = Some(format!("rate_limited until soon: {REASON}"));
    assert_eq!(
        text(&[job_status(&waiting, 0, 3_600, &look)]),
        "! Needs a pool"
    );
    assert_eq!(
        crate::tui::seam::ResearchCounts::of(&[waiting]).summary(),
        "Research: needs a pool"
    );
}

#[test]
fn after_a_stopped_master_turn_the_help_names_the_keys_the_tab_takes() {
    let mut screen = ProjectScreen::new(
        ProjectId::new("notes-cli").unwrap(),
        "Notes CLI".into(),
        None,
    );
    screen.pool = Some(PoolInfo {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        accounts: 1,
        assigned: true,
    });
    screen.transcript = Some(Transcript {
        harness: "RCLI".into(),
        account: None,
        tools: true,
        open: true,
        status: MasterStatus::Stopped,
        messages: Vec::new(),
    });
    screen.open(Tab::Conversation);
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(screen.typing, "the Conversation tab opens on its input");
    assert!(
        drawn.help.contains(&("Esc", "Leave input")),
        "{:?}",
        drawn.help
    );
    assert!(
        !drawn.help.contains(&("r", "Try again")),
        "{:?}",
        drawn.help
    );
    assert!(
        !drawn.help.contains(&("e", "End conversation")),
        "{:?}",
        drawn.help
    );
    assert_eq!(screen.key(key(KeyCode::Char('r'))), ProjectReply::Handled);
    assert_eq!(
        screen.input.value(),
        "r",
        "r types while the input has the keys"
    );

    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(drawn.help.contains(&("r", "Try again")), "{:?}", drawn.help);
    assert!(
        drawn.help.contains(&("e", "End conversation")),
        "{:?}",
        drawn.help
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('e'))),
        ProjectReply::EndConversation
    );
    screen
        .transcript
        .as_mut()
        .expect("the transcript is set above")
        .open = false;
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(
        !drawn.help.contains(&("e", "End conversation")),
        "{:?}",
        drawn.help
    );
    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Ignored);
}

#[test]
fn the_agents_tab_shows_only_the_newest_output_lines_that_fit() {
    let project = ProjectId::new("tidepool").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Tidepool".into(), None);
    screen.jobs = Some(vec![ResearchJob {
        id: 7,
        project_id: project,
        kind: ResearchKind::MarketResearch,
        kind_label: "Market research".into(),
        status: ResearchStatus::Running,
        status_label: "Running".into(),
        report_path: "context/research/market_research.md".into(),
        harness: "codex".into(),
        tools: true,
        attempts: 1,
        claimer: Some("runner".into()),
        error: None,
        cancel_requested: false,
        output_bytes: 2048,
        created_ms: 1_000,
        started_ms: Some(2_000),
        heartbeat_ms: Some(3_000),
        finished_ms: None,
    }]);
    screen.open(Tab::Agents);
    let look = Look::plain();
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(shown.contains("Reading the agent's output..."), "{shown}");

    let mut lines: Vec<String> = (0..40).map(|n| format!("step {n}")).collect();
    lines.push(format!("{}end", "x".repeat(120)));
    screen.log = Some(JobLog {
        job: 7,
        status: ResearchStatus::Running,
        lines: Some(lines),
        read_ms: 3_000,
    });
    for (width, height) in [(80u16, 24u16), (120, 36)] {
        let drawn = project_screen(&mut screen, width, height, &look);
        assert_eq!(drawn.rows.len(), usize::from(height) - 4);
        assert!(
            drawn
                .rows
                .iter()
                .all(|row| row.width() == usize::from(width))
        );
        let shown = text(&drawn.rows);
        assert!(shown.contains("step 39"), "{shown}");
        assert!(
            !drawn
                .rows
                .iter()
                .any(|row| row.text().trim_end().ends_with("step 0")),
            "the oldest lines give way to the newest: {shown}"
        );
        assert!(shown.contains("x\u{2026}"), "a long line is cut: {shown}");
        assert!(!shown.contains("xend"), "{shown}");
    }

    screen.log.as_mut().unwrap().lines = Some(Vec::new());
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(shown.contains("No output yet."), "{shown}");
    screen.log.as_mut().unwrap().lines = None;
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("The agent's output could not be read."),
        "{shown}"
    );
    screen.log.as_mut().unwrap().job = 8;
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Reading the agent's output..."),
        "another job's lines are never shown: {shown}"
    );
}

fn stored(project: &ProjectId) -> StoredSetup {
    StoredSetup {
        project_id: project.clone(),
        name: "Notes CLI".into(),
        about: "Terminal-first notes.".into(),
        tech_stack: vec!["rust".into(), "sqlite".into()],
        platforms: vec![Platform::Cli],
        research: vec![
            ResearchKind::TechStackResearch,
            ResearchKind::CompetitorResearch,
        ],
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
    }
}

fn loaded(project: &ProjectId) -> ProjectScreen {
    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let first = screen.next_query(10).unwrap();
    assert_eq!(
        first,
        SetupQuery::Setup {
            project: project.clone()
        }
    );
    screen.apply(
        &first,
        Ok(SetupView::Setup(Some(Box::new(stored(project))))),
    );
    assert_eq!(screen.name, "Notes CLI");
    screen
}

#[test]
fn the_add_file_box_checks_the_path_before_the_engine_copies_it() {
    let project = ProjectId::new("files").unwrap();
    let mut screen =
        ProjectScreen::new(project.clone(), "Files".into(), Some("/Users/owner".into()));
    screen.open(Tab::Context);
    assert_eq!(screen.key(key(KeyCode::Char('a'))), ProjectReply::Handled);
    assert!(screen.adding.is_some());
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.adding.as_ref().unwrap().error.as_deref(),
        Some("Type the full path of a file on this computer.")
    );
    assert_eq!(screen.paste("notes/scope.csv"), ProjectReply::Handled);
    assert_eq!(screen.adding.as_ref().unwrap().error, None);
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.adding.as_ref().unwrap().error.as_deref(),
        Some("Use the full path of a file, starting with / or ~/.")
    );
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.adding.is_none());

    assert_eq!(screen.key(key(KeyCode::Char('a'))), ProjectReply::Handled);
    assert_eq!(screen.paste("~/notes/scope.csv"), ProjectReply::Handled);
    let add = SetupAction::AddDocuments {
        project_id: project.clone(),
        documents: vec!["/Users/owner/notes/scope.csv".into()],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(add.clone()))
    );
    assert!(screen.adding.as_ref().unwrap().busy);
    assert_eq!(screen.key(key(KeyCode::Char('x'))), ProjectReply::Handled);
    assert_eq!(screen.paste("more"), ProjectReply::Ignored);
    assert_eq!(
        screen.adding.as_ref().unwrap().input.value(),
        "~/notes/scope.csv",
        "the path waits while the engine copies it"
    );

    let change = SetupQuery::Change {
        action: add.clone(),
        generation: 1,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "document_not_found".into(),
        })),
    );
    let adding = screen.adding.as_ref().unwrap();
    assert!(!adding.busy);
    assert_eq!(
        adding.error.as_deref(),
        Some("There is no file at that path. Check it and try again.")
    );
    assert_eq!(adding.input.value(), "~/notes/scope.csv");
    assert_eq!(screen.problem, None);

    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(add))
    );
    screen.apply(&change, Err("trusted_connection_unavailable".into()));
    assert_eq!(
        screen.adding.as_ref().unwrap().error.as_deref(),
        Some("Not connected to the engine. Retrying.")
    );
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(SetupAction::AddDocuments {
            project_id: project.clone(),
            documents: vec!["/Users/owner/notes/scope.csv".into()],
        }))
    );
    screen.apply(&change, Ok(SetupView::Changed(OwnerResult::Applied)));
    assert!(screen.adding.is_none());
    assert_eq!(screen.problem, None);

    let mut homeless = ProjectScreen::new(project, "Files".into(), None);
    homeless.open(Tab::Context);
    homeless.key(key(KeyCode::Char('a')));
    homeless.paste("~/notes/scope.csv");
    assert_eq!(homeless.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        homeless.adding.as_ref().unwrap().error.as_deref(),
        Some("Use the full path of a file, starting with / or ~/.")
    );
}

#[test]
fn the_general_form_saves_one_edit_at_the_revision_it_opened() {
    let project = ProjectId::new("general").unwrap();
    let mut early = ProjectScreen::new(project.clone(), "General".into(), None);
    early.open(Tab::Settings);
    assert_eq!(early.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    assert!(early.general.is_none());
    assert_eq!(
        early.problem.as_deref(),
        Some("Still loading the project. Try again.")
    );

    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    let general = screen.general.as_ref().unwrap();
    assert_eq!(general.name.value(), "Notes CLI");
    assert_eq!(general.about.value(), "Terminal-first notes.");
    assert_eq!(general.stack.value(), "rust, sqlite");
    assert_eq!(general.revision, 4);
    assert_eq!(general.cursor, 0);
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Handled,
        "nothing changed, so nothing is sent"
    );
    assert!(screen.general.is_none());

    assert_eq!(screen.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    let control_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(screen.key(control_u), ProjectReply::Handled);
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.general.as_ref().unwrap().error.as_deref(),
        Some("Enter a project name of 1 to 256 characters.")
    );
    assert_eq!(screen.paste("Notes Pro"), ProjectReply::Handled);
    assert_eq!(screen.general.as_ref().unwrap().error, None);

    assert_eq!(screen.key(key(KeyCode::Char(' '))), ProjectReply::Handled);
    assert_eq!(screen.general.as_ref().unwrap().name.value(), "Notes Pro ");
    assert_eq!(screen.key(key(KeyCode::Backspace)), ProjectReply::Handled);
    screen.key(key(KeyCode::Tab));
    screen.key(key(KeyCode::Tab));
    assert_eq!(screen.general.as_ref().unwrap().cursor, 2);
    assert_eq!(screen.paste(", tauri, ,"), ProjectReply::Handled);
    screen.key(key(KeyCode::Down));
    assert_eq!(screen.key(key(KeyCode::Char(' '))), ProjectReply::Handled);
    for _ in 0..3 {
        screen.key(key(KeyCode::Tab));
    }
    assert_eq!(screen.general.as_ref().unwrap().cursor, 6);
    screen.key(key(KeyCode::Char(' ')));
    for _ in 0..4 {
        screen.key(key(KeyCode::Tab));
    }
    assert_eq!(screen.general.as_ref().unwrap().cursor, 10);
    screen.key(key(KeyCode::Char(' ')));
    assert_eq!(
        screen.general.as_ref().unwrap().platforms,
        [Platform::Website]
    );
    assert_eq!(
        screen.general.as_ref().unwrap().research,
        [ResearchKind::TechStackResearch]
    );
    for _ in 0..20 {
        screen.key(key(KeyCode::Tab));
    }
    assert_eq!(screen.general.as_ref().unwrap().cursor, 13);
    screen.key(shifted(KeyCode::BackTab));
    screen.key(key(KeyCode::Up));
    assert_eq!(screen.general.as_ref().unwrap().cursor, 11);

    let update = SetupAction::Update {
        project_id: project.clone(),
        expected_revision: 4,
        name: Some("Notes Pro".into()),
        setup: ProjectSetup {
            about: "Terminal-first notes.".into(),
            tech_stack: vec!["rust".into(), "sqlite".into(), "tauri".into()],
            platforms: vec![Platform::Website],
            research: vec![ResearchKind::TechStackResearch],
            pool: None,
            team: Some(stored(&project).team),
            documents: vec![],
        },
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(update.clone()))
    );
    assert!(screen.general.as_ref().unwrap().busy);
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert_eq!(screen.general.as_ref().unwrap().cursor, 11);

    let change = SetupQuery::Change {
        action: update.clone(),
        generation: 2,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "project_setup_revision".into(),
        })),
    );
    let general = screen.general.as_ref().unwrap();
    assert!(!general.busy);
    assert_eq!(
        general.error.as_deref(),
        Some("This project changed since you opened the form. Open it again.")
    );
    assert_eq!(screen.problem, None);
    assert_eq!(
        screen.next_query(20),
        Some(SetupQuery::Setup {
            project: project.clone()
        }),
        "an edit always re-reads the setup"
    );

    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(update))
    );
    screen.apply(&change, Ok(SetupView::Changed(OwnerResult::Applied)));
    assert!(screen.general.is_none());
    assert_eq!(screen.problem, None);

    screen.key(key(KeyCode::Char('g')));
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.general.is_none());
}

#[test]
fn remove_asks_first_and_marks_the_project_removed_once_the_engine_forgets_it() {
    let project = ProjectId::new("gone").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Gone".into(), None);
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('x'))), ProjectReply::Handled);
    assert!(screen.confirm_remove);
    assert_eq!(screen.key(key(KeyCode::Char('q'))), ProjectReply::Handled);
    assert!(screen.confirm_remove);
    assert_eq!(screen.key(key(KeyCode::Char('n'))), ProjectReply::Handled);
    assert!(!screen.confirm_remove);
    screen.key(key(KeyCode::Char('x')));
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(!screen.confirm_remove);

    screen.key(key(KeyCode::Char('x')));
    let shown = text(&project_screen(&mut screen, 80, 24, &Look::plain()).rows);
    assert!(
        shown.contains("Its master conversation ends,"),
        "the confirm says removing ends the master conversation: {shown}"
    );
    let remove = SetupAction::Remove {
        project_id: project.clone(),
    };
    assert_eq!(
        screen.key(key(KeyCode::Char('y'))),
        ProjectReply::Change(Box::new(remove.clone()))
    );
    assert!(!screen.confirm_remove);
    let change = SetupQuery::Change {
        action: remove,
        generation: 3,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "invalid_project".into(),
        })),
    );
    assert!(!screen.removed);
    assert_eq!(
        screen.problem.as_deref(),
        Some("This project no longer exists.")
    );
    screen.apply(&change, Ok(SetupView::Changed(OwnerResult::Applied)));
    assert!(screen.removed);

    screen.open(Tab::Agents);
    assert_eq!(screen.key(key(KeyCode::Char('t'))), ProjectReply::Team);
}

#[test]
fn the_add_file_box_general_form_and_remove_question_draw_with_their_keys() {
    let look = Look::plain();
    let project = ProjectId::new("drawn").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Context);
    screen.key(key(KeyCode::Char('a')));
    screen.paste("/Users/owner/notes/scope.csv");
    for (width, height) in [(120u16, 36u16), (80, 24)] {
        let drawn = project_screen(&mut screen, width, height, &look);
        assert!(
            drawn
                .rows
                .iter()
                .all(|row| row.width() == usize::from(width))
        );
        let shown = text(&drawn.rows);
        assert!(shown.contains("Add file"), "{shown}");
        assert!(shown.contains("Type the full path of a file"), "{shown}");
        assert!(shown.contains("/Users/owner/notes/scope.csv"), "{shown}");
        assert!(shown.contains("[> Add file ]"), "{shown}");
        assert_eq!(drawn.help, [("Enter", "Add file"), ("Esc", "Cancel")]);
        assert!(drawn.cursor.is_some());
    }
    screen.key(key(KeyCode::Enter));
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("Adding..."));
    assert_eq!(drawn.help, [("Esc", "Close")]);
    assert_eq!(drawn.cursor, None);
    screen.key(key(KeyCode::Esc));

    screen.open(Tab::Settings);
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("Notes CLI   g Edit details"), "{shown}");
    assert!(shown.contains("x Remove project"), "{shown}");

    screen.key(key(KeyCode::Char('g')));
    for (width, height) in [(120u16, 36u16), (80, 24)] {
        let drawn = project_screen(&mut screen, width, height, &look);
        let shown = text(&drawn.rows);
        assert!(shown.contains("Edit project"), "{shown}");
        assert!(shown.contains("[ Notes CLI"), "{shown}");
        assert!(shown.contains("[ rust, sqlite"), "{shown}");
        assert!(shown.contains("[x] CLI"), "{shown}");
        assert!(shown.contains("[ ] Website"), "{shown}");
        assert!(shown.contains("[x] Competitor research"), "{shown}");
        assert!(shown.contains("[ ] Market research"), "{shown}");
        assert_eq!(
            drawn.help,
            [("Enter", "Save"), ("Tab", "Next"), ("Esc", "Cancel")]
        );
        assert!(drawn.cursor.is_some());
    }
    screen.key(key(KeyCode::Tab));
    assert_eq!(
        project_screen(&mut screen, 80, 24, &look).help,
        [
            ("Enter", "Save"),
            ("Tab", "Next"),
            ("Ctrl-J", "New line"),
            ("Esc", "Cancel")
        ]
    );
    screen.key(key(KeyCode::Tab));
    screen.key(key(KeyCode::Tab));
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert_eq!(
        drawn.help,
        [
            ("Enter", "Save"),
            ("Tab", "Next"),
            ("Space", "Switch"),
            ("Esc", "Cancel")
        ]
    );
    assert_eq!(drawn.cursor, None);
    screen.key(key(KeyCode::Char(' ')));
    screen.key(key(KeyCode::Enter));
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("Saving..."));
    assert_eq!(drawn.cursor, None);
    screen.key(key(KeyCode::Esc));

    screen.key(key(KeyCode::Char('x')));
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(
        shown.contains("Remove Notes CLI from CrazyAgent?"),
        "{shown}"
    );
    assert!(shown.contains("[ Remove project ]"), "{shown}");
    assert!(shown.contains("[> Keep project ]"), "{shown}");
    assert_eq!(
        drawn.help,
        [("y", "Remove"), ("n", "Keep"), ("Esc", "Keep")]
    );
}

#[tokio::test]
async fn failed_and_empty_reads_settle_back_off_and_say_why() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-quiet-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("empty.md"), "").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
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
    let _home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let project = ProjectId::new("quiet-notes").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Quiet notes".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Notes that load once.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Cli],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![sources.path().join("empty.md").to_str().unwrap().into()],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));
    let look = Look::plain();
    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let start = 1_793_000_000_000;
    assert_eq!(
        settle(&mut screen, &mut controller, start).await,
        ["setup", "jobs", "context", "memories"]
    );

    screen.read("context/documents/empty.md");
    assert_eq!(
        settle(&mut screen, &mut controller, start + 100).await,
        ["item"],
        "an empty file is read once"
    );
    let shown = text(&project_screen(&mut screen, 120, 36, &look).rows);
    assert!(shown.contains("This file is empty."), "{shown}");
    assert!(!shown.contains("Loading..."), "{shown}");

    screen.read("context/documents/gone.md");
    assert_eq!(
        settle(&mut screen, &mut controller, start + 200).await,
        ["item"],
        "a missing file is asked for once"
    );
    assert_eq!(
        screen.problem.as_deref(),
        Some("That file is already gone.")
    );
    screen.problem = None;
    assert!(
        settle(&mut screen, &mut controller, start + 200 + REFRESH_MS)
            .await
            .iter()
            .all(|read| *read != "item"),
        "a missing file is not asked for again"
    );
    assert_eq!(screen.problem, None);
    let shown = text(&project_screen(&mut screen, 120, 36, &look).rows);
    assert!(shown.contains("That file is already gone."), "{shown}");
    assert!(!shown.contains("Loading..."), "{shown}");
    screen.read("context/documents/gone.md");
    assert!(
        settle(&mut screen, &mut controller, start + 300 + REFRESH_MS)
            .await
            .contains(&"item"),
        "opening a failed file again asks for it again"
    );
    screen.problem = None;

    tokio::time::timeout(std::time::Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let later = start + 10 * REFRESH_MS;
    screen.open(Tab::Context);
    assert_eq!(
        settle(&mut screen, &mut controller, later).await,
        ["context"],
        "a failed context read waits before it is sent again"
    );
    assert!(screen.problem.take().is_some(), "the failure is shown");
    assert!(
        settle(&mut screen, &mut controller, later + 10)
            .await
            .is_empty()
    );
    assert_eq!(
        settle(&mut screen, &mut controller, later + REFRESH_MS).await,
        ["context"]
    );

    let mut fresh = ProjectScreen::new(project.clone(), "loading".into(), None);
    assert_eq!(
        settle(&mut fresh, &mut controller, later).await,
        ["setup"],
        "a failed setup read waits instead of starving every other read"
    );
    assert!(fresh.problem.is_some());
    assert!(
        settle(&mut fresh, &mut controller, later + 10)
            .await
            .is_empty()
    );
    assert_eq!(
        settle(&mut fresh, &mut controller, later + REFRESH_MS).await,
        ["setup"]
    );
}
