use super::new_project_view::{Look, Row};
use super::project::{
    JobLog, MasterStatus, PoolInfo, ProjectReply, ProjectScreen, REFRESH_MS, RemoveChoice, Tab,
    Transcript, memory_source, plain_problem, research_kind,
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
use crate::sandbox::policy::{AgentAccess, AgentPolicy, LevelView, PolicyView};
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
        SetupQuery::ReadPaths { project } => SetupView::ReadPaths(
            controller
                .project_read_paths(project)
                .await
                .map_err(problem)?,
        ),
        SetupQuery::HiddenPaths { project } => SetupView::HiddenPaths(
            controller
                .project_hidden_paths(project)
                .await
                .map_err(problem)?,
        ),
        SetupQuery::AgentPolicy { project } => SetupView::AgentPolicy(
            controller
                .project_agent_policy(project)
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
                SetupAction::PlanRoadmap { project_id } => {
                    controller.plan_roadmap(project_id, generation).await
                }
                SetupAction::Update {
                    project_id,
                    expected_revision,
                    name,
                    setup,
                    ..
                } => {
                    controller
                        .update_project_setup(
                            project_id,
                            expected_revision,
                            name,
                            setup,
                            generation,
                        )
                        .await
                }
                other => panic!("the project screen never sends {other:?}"),
            }
            .map_err(problem)?,
        )?,
        SetupQuery::Removal { project } => SetupView::Removal(Box::new(
            controller.removal_preview(project).await.map_err(problem)?,
        )),
        SetupQuery::Overview { project } => SetupView::Overview(Box::new(
            controller.overview_read(project).await.map_err(problem)?,
        )),
        SetupQuery::RefreshOverview { project } => {
            controller
                .overview_refresh(project)
                .await
                .map_err(problem)?;
            SetupView::OverviewAsked
        }
        SetupQuery::Catalog { .. } => panic!("the project screen never reads the catalog"),
        SetupQuery::Team { .. }
        | SetupQuery::TeamChange { .. }
        | SetupQuery::TeamMessages { .. }
        | SetupQuery::OpenLiveSession { .. }
        | SetupQuery::MoveHome { .. }
        | SetupQuery::Library(_)
        | SetupQuery::CoordQuestions { .. } => {
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
            SetupQuery::Overview { .. } => "overview",
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
        ["setup", "jobs", "overview", "context", "memories"]
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
    assert!(
        screen.edit.is_none() && screen.review.is_some(),
        "e must show the whole memory before its editor opens"
    );
    for _ in 0..40 {
        assert_eq!(screen.key(key(KeyCode::PageDown)), ProjectReply::Handled);
    }
    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(screen.edit.is_some());
    screen.edit = None;
    screen.preview = None;
    screen.reading = false;
    screen.open(Tab::Overview);
    screen.width = 120;

    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(
        screen.edit.is_none(),
        "e must show the whole memory before its editor opens"
    );
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(text(&drawn.rows).contains("# Competitors"));
    assert!(
        !drawn.help.contains(&("e", "Edit first")),
        "{:?}",
        drawn.help
    );
    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(
        screen.edit.is_none(),
        "e edits only once the last line was on screen"
    );
    for _ in 0..40 {
        assert_eq!(screen.key(key(KeyCode::PageDown)), ProjectReply::Handled);
    }
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(
        drawn.help.contains(&("e", "Edit first")),
        "{:?}",
        drawn.help
    );
    assert_eq!(screen.key(key(KeyCode::Char('e'))), ProjectReply::Handled);
    assert!(screen.edit.is_some());
    assert!(screen.review.is_none());
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
    assert_eq!(screen.input.value(), "", "Esc clears the prompt first");
    assert!(screen.typing);
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
fn in_the_master_input_shift_enter_and_ctrl_j_add_a_line_and_enter_never_does() {
    let mut screen = ProjectScreen::new(ProjectId::new("keys").unwrap(), "Keys".into(), None);
    screen.open(Tab::Conversation);
    assert!(screen.typing);
    assert_eq!(screen.key(key(KeyCode::Char('a'))), ProjectReply::Handled);
    assert_eq!(screen.key(shifted(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(screen.key(key(KeyCode::Char('b'))), ProjectReply::Handled);
    assert_eq!(
        screen.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        ProjectReply::Handled
    );
    assert_eq!(screen.input.value(), "a\nb\n");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.input.value(),
        "a\nb\n",
        "Enter is the send key, never a new line; with no pool nothing is sent"
    );
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
fn every_engine_refusal_for_agent_access_reads_its_own_sentence() {
    let families = [
        "agent_access_",
        "agent_policy_",
        "agent_network_",
        "agent_toolchain_",
        "agent_project_",
    ];
    let mut missing = Vec::new();
    for source in [
        include_str!("../../sandbox/policy.rs"),
        include_str!("../../project_setup.rs"),
        include_str!("../../storage.rs"),
    ] {
        for code in source.split('"').filter(|text| {
            families.iter().any(|family| text.starts_with(family))
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }) {
            if plain_problem(code).contains("(code: ") && !missing.contains(&code) {
                missing.push(code);
            }
        }
    }
    assert_eq!(
        missing,
        Vec::<&str>::new(),
        "every access, policy, network, toolchain and project refusal has a sentence"
    );
    for (code, sentence) in [
        (
            "agent_access_lowered",
            "This project's agent access was lowered below what the agent started with, so it was stopped. What it changed in its own folder is still in Changes; an agent that ran at Full access may also have changed files outside it.",
        ),
        (
            "agent_network_narrowed",
            "This project's agent network was narrowed, so the agent was stopped. Its next start uses the new network.",
        ),
        (
            "agent_toolchain_lowered",
            "A toolchain the agent started with is no longer allowed for this project, so the agent was stopped. Its next start goes without it.",
        ),
        (
            "agent_project_unreadable",
            "CrazyAgent couldn't read the project folder to check the agent's toolchains, so the agent was stopped. Try again.",
        ),
    ] {
        assert_eq!(
            plain_problem(code),
            sentence,
            "{code} reads as on the desktop"
        );
    }
}

/// TN-L1: every cause a process stops for under a narrowed policy reads its own sentence,
/// word for word the same in the TUI and in the desktop's copy table. The walk follows one
/// match with no catch-all arm, whose arm for each Narrowing names its code and the next
/// Narrowing, so a new one fails to compile here until it has an arm, and that arm puts it
/// on the walk, where its sentence is checked, whatever its code's family and wherever that
/// code is defined (TN-L1 r1).
#[test]
fn every_narrowing_stop_reads_the_same_sentence_in_both_uis() {
    use crate::sandbox::policy::Narrowing;
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/desktop/src/api.ts"
    ))
    .unwrap();
    let start = source
        .find("const COPY: Record<string, string> = {")
        .unwrap();
    let body = &source[start..];
    let body = &body[..body.find("\n};").unwrap()];
    let desktop: std::collections::BTreeMap<&str, &str> = body
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (code, sentence) = line.trim().split_once(": ")?;
            Some((
                code,
                sentence
                    .strip_suffix(',')?
                    .strip_prefix('"')?
                    .strip_suffix('"')?,
            ))
        })
        .collect();
    let mut walked = Vec::new();
    let mut next = Some(Narrowing::Access);
    while let Some(narrowing) = next {
        assert!(
            !walked.contains(&narrowing),
            "the walk reaches {narrowing:?} once"
        );
        walked.push(narrowing);
        let (code, after) = match narrowing {
            Narrowing::Access => ("agent_access_lowered", Some(Narrowing::Network)),
            Narrowing::Network => ("agent_network_narrowed", Some(Narrowing::TurnedOff)),
            Narrowing::TurnedOff => ("agent_toolchain_lowered", Some(Narrowing::NotAllowed)),
            Narrowing::NotAllowed => ("agent_toolchain_lowered", Some(Narrowing::PolicyUnreadable)),
            Narrowing::PolicyUnreadable => {
                ("agent_policy_unreadable", Some(Narrowing::FolderUnreadable))
            }
            Narrowing::FolderUnreadable => ("agent_project_unreadable", None),
        };
        next = after;
        assert_eq!(narrowing.code(), code, "{narrowing:?} reports its code");
        let sentence = plain_problem(code);
        assert!(
            !sentence.contains("(code: "),
            "{code} has its own sentence in the TUI: {sentence}"
        );
        assert_eq!(
            desktop.get(code).copied(),
            Some(sentence.as_str()),
            "{code} reads the same on the desktop"
        );
    }
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
        ["setup", "jobs", "overview", "context", "memories"],
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
        ["setup", "jobs", "log"],
        "a retry may turn its research back on, so the setup is read again"
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
fn with_a_live_master_the_conversation_tab_stays_the_console_and_slash_master_opens_its_terminal() {
    let project = ProjectId::new("notes-cli").unwrap();
    let mut screen = loaded(&project);
    screen.pool = Some(PoolInfo {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        accounts: 1,
        assigned: true,
    });
    screen.transcript = Some(Transcript {
        harness: "Codex".into(),
        account: None,
        tools: true,
        open: true,
        status: MasterStatus::Stopped,
        messages: Vec::new(),
    });
    screen.open(Tab::Conversation);
    assert!(screen.typing, "the console takes typing for every master");
    let drawn = project_screen(&mut screen, 80, 24, &Look::plain());
    let shown = text(&drawn.rows);
    assert!(shown.contains("Message the master"), "{shown}");
    assert!(!shown.contains("Read-only"), "{shown}");
    type_text(&mut screen, "/master");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Master);
    assert_eq!(screen.input.value(), "");

    screen.setup.as_mut().expect("loaded").team.master = "rcli".into();
    type_text(&mut screen, "/master");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.console_note,
        ["This master has no live terminal; its conversation is this console."]
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
    assert_eq!(screen.input.value(), "", "the first Esc clears the prompt");
    assert!(screen.typing);
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
fn a_wide_conversation_shows_the_team_panel_beside_the_master_and_t_opens_the_team() {
    let project = ProjectId::new("tidepool").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Tidepool".into(), None);
    screen.team = crate::tui::ctx::Load::Ready(Box::new(crate::team::TeamGraph {
        project_id: project,
        nodes: vec![crate::team::GraphNode {
            id: "team-1".into(),
            kind: crate::team::NodeKind::Lead,
            parent: Some("master".into()),
            label: "Ada (@onboarding-fix)".into(),
            harness: "codex".into(),
            status: Some(crate::team::AgentStatus::Working),
            status_label: None,
            account: None,
            branch: None,
            terminal: Some(crate::terminal::AgentId::new("team-1").unwrap()),
            questions: None,
            agent: Some(1),
            pane: None,
            presence: None,
            waiting: None,
            waits_for: None,
            owner_questions: 0,
            no_worktree: false,
            hookless: false,
            message_note: None,
            started_ms: None,
        }],
        edges: Vec::new(),
        messages: Vec::new(),
        overlaps: Vec::new(),
        relations: Vec::new(),
        master_live_terminal: true,
    }));
    screen.open(Tab::Conversation);
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 140, 24, &look);
    assert!(screen.typing, "the Conversation tab opens on its input");
    assert!(drawn.rows.iter().all(|row| row.width() == 140));
    let panel = 2 + 136 - 32;
    let column = |needle: &str| {
        drawn.rows.iter().find_map(|row| {
            let text = row.text();
            text.find(needle).map(|at| text[..at].chars().count())
        })
    };
    assert_eq!(column("Team"), Some(panel), "{}", text(&drawn.rows));
    assert_eq!(column("\u{25b8} Ada (@onboarding-fix)"), Some(panel));
    assert_eq!(column("  Codex, Working"), Some(panel));
    assert!(
        !drawn.help.contains(&("t", "Team")),
        "t types while the input has the keys"
    );
    assert_eq!(screen.key(key(KeyCode::Char('t'))), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "t");

    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "", "the first Esc clears the prompt");
    assert!(screen.typing);
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    let drawn = project_screen(&mut screen, 140, 24, &look);
    assert!(drawn.help.contains(&("t", "Team")), "{:?}", drawn.help);
    assert_eq!(screen.key(key(KeyCode::Char('t'))), ProjectReply::Team);

    let narrow = text(&project_screen(&mut screen, 90, 24, &look).rows);
    assert!(!narrow.contains("Ada (@onboarding-fix)"), "{narrow}");
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

#[test]
fn a_research_job_queued_again_shows_why_and_is_not_drawn_as_running() {
    const REASON: &str = "A toolchain this research ran with was turned off, so it was stopped and queued again; its next run goes without it";
    let project = ProjectId::new("tidepool").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Tidepool".into(), None);
    screen.now_ms = 600_000;
    screen.jobs = Some(vec![ResearchJob {
        id: 7,
        project_id: project,
        kind: ResearchKind::MarketResearch,
        kind_label: "Market research".into(),
        status: ResearchStatus::Queued,
        status_label: "Queued".into(),
        report_path: "context/research/market_research.md".into(),
        harness: "codex".into(),
        tools: true,
        attempts: 0,
        claimer: None,
        error: Some(REASON.into()),
        cancel_requested: false,
        output_bytes: 2048,
        created_ms: 1_000,
        started_ms: Some(2_000),
        heartbeat_ms: Some(3_000),
        finished_ms: None,
    }]);
    screen.open(Tab::Agents);
    let look = Look::plain();
    for (width, height) in [(80u16, 24u16), (120, 36)] {
        let shown = text(&project_screen(&mut screen, width, height, &look).rows);
        let words = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains(REASON), "{shown}");
        assert!(!shown.contains("running"), "{shown}");
        assert!(
            shown
                .lines()
                .any(|line| line.trim_start().starts_with("Market research")
                    && line.contains("Started")),
            "{shown}"
        );
    }
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

    let update_at = |expected_revision: u64| SetupAction::Update {
        project_id: project.clone(),
        kept: None,
        expected_revision,
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
    let update = update_at(4);
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
            code: crate::storage::versioned::STALE_CODE.into(),
        })),
    );
    let general = screen.general.as_ref().unwrap();
    assert!(!general.busy);
    assert!(general.stale);
    assert_eq!(
        general.error.as_deref(),
        Some(
            "This project changed since you opened the form. Your edit is kept while the saved one is read."
        )
    );
    assert_eq!(screen.problem, None);
    let read = screen.next_query(20);
    assert_eq!(
        read,
        Some(SetupQuery::Setup {
            project: project.clone()
        }),
        "an edit always re-reads the setup"
    );
    let mut elsewhere = stored(&project);
    elsewhere.name = "Notes Server".into();
    elsewhere.revision = 5;
    screen.apply(
        &read.unwrap(),
        Ok(SetupView::Setup(Some(Box::new(elsewhere)))),
    );
    let general = screen.general.as_ref().unwrap();
    assert_eq!(general.name.value(), "Notes Pro", "the typed draft is kept");
    assert_eq!(general.stack.value(), "rust, sqlite, tauri, ,");
    assert_eq!(general.revision, 5, "the form takes the saved revision");
    assert!(!general.stale);
    assert_eq!(
        general.error.as_deref(),
        Some("Changed elsewhere: name. Enter saves your edit over it; Esc keeps the saved one."),
        "the field changed elsewhere is named before Enter saves over it"
    );
    let drawn = text(&project_screen(&mut screen, 80, 24, &Look::plain()).rows);
    assert!(drawn.contains("Enter saves your edit over it"), "{drawn}");

    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(update_at(5))),
        "Enter keeps the owner's edit over the saved one"
    );
    screen.apply(&change, Ok(SetupView::Changed(OwnerResult::Applied)));
    assert!(screen.general.is_none());
    assert_eq!(screen.problem, None);

    screen.key(key(KeyCode::Char('g')));
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.general.is_none());
}

#[test]
fn a_general_save_takes_the_re_read_revision_only_after_a_stale_refusal() {
    let project = ProjectId::new("general-stale").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    screen.key(key(KeyCode::Char('g')));
    let control_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    screen.key(control_u);
    assert_eq!(screen.paste("Notes Pro"), ProjectReply::Handled);
    let ProjectReply::Change(sent) = screen.key(key(KeyCode::Enter)) else {
        panic!("the edit is sent");
    };
    let SetupAction::Update {
        expected_revision, ..
    } = &*sent
    else {
        panic!("the General form sends a setup update");
    };
    assert_eq!(*expected_revision, 4);
    let update = *sent;
    let change = SetupQuery::Change {
        action: update.clone(),
        generation: 2,
    };

    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "project_about_invalid".into(),
        })),
    );
    assert!(!screen.general.as_ref().unwrap().stale);
    let read = screen.next_query(20).expect("an edit re-reads the setup");
    let mut elsewhere = stored(&project);
    elsewhere.revision = 5;
    screen.apply(&read, Ok(SetupView::Setup(Some(Box::new(elsewhere)))));
    let general = screen.general.as_ref().unwrap();
    assert_eq!(
        general.revision, 4,
        "a refusal that is not stale keeps the revision the form opened at"
    );
    assert_eq!(
        general.error.as_deref(),
        Some("The description is too long or has characters that can't be saved.")
    );

    let ProjectReply::Change(sent) = screen.key(key(KeyCode::Enter)) else {
        panic!("the edit is sent again");
    };
    assert_eq!(
        *sent, update,
        "the form still saves at the revision it opened at"
    );
    screen.apply(&change, Err("stale_revision".into()));
    let general = screen.general.as_ref().unwrap();
    assert!(general.stale, "a stale refusal by error is stale too");
    assert!(!general.busy);
    assert_eq!(
        general.error.as_deref(),
        Some(
            "This project changed since you opened the form. Your edit is kept while the saved one is read."
        )
    );
    assert_eq!(screen.problem, None);
    let read = screen
        .next_query(30)
        .expect("a stale refusal reads the setup again");
    assert_eq!(
        read,
        SetupQuery::Setup {
            project: project.clone()
        }
    );
    let mut elsewhere = stored(&project);
    elsewhere.revision = 6;
    screen.apply(&read, Ok(SetupView::Setup(Some(Box::new(elsewhere)))));
    let general = screen.general.as_ref().unwrap();
    assert_eq!(general.revision, 6);
    assert_eq!(general.name.value(), "Notes Pro");
    assert_eq!(
        general.error.as_deref(),
        Some(
            "This project changed elsewhere, but not on this form. Enter saves your edit; Esc keeps the saved one."
        ),
        "a change outside the form's fields names none"
    );

    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.general.is_none(), "Esc keeps the saved one");
    screen.key(key(KeyCode::Char('g')));
    let general = screen.general.as_ref().unwrap();
    assert_eq!(general.name.value(), "Notes CLI");
    assert_eq!((general.revision, general.stale), (6, false));
    assert_eq!(general.error, None);
}

#[test]
fn a_stale_general_form_names_each_field_changed_since_the_owner_last_saw_it() {
    let project = ProjectId::new("general-elsewhere").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    screen.key(key(KeyCode::Char('g')));
    let control_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    screen.key(control_u);
    assert_eq!(screen.paste("Notes Pro"), ProjectReply::Handled);
    let ProjectReply::Change(sent) = screen.key(key(KeyCode::Enter)) else {
        panic!("the edit is sent");
    };
    let change = SetupQuery::Change {
        action: *sent,
        generation: 2,
    };
    let stale = || {
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: crate::storage::versioned::STALE_CODE.into(),
        }))
    };

    screen.apply(&change, stale());
    let read = screen
        .next_query(20)
        .expect("a stale refusal reads the setup again");
    let mut elsewhere = stored(&project);
    elsewhere.revision = 5;
    elsewhere.about = "Shared notes.".into();
    elsewhere.platforms = vec![Platform::Website, Platform::Cli];
    elsewhere.research.push(ResearchKind::RoadmapPlanning);
    screen.apply(
        &read,
        Ok(SetupView::Setup(Some(Box::new(elsewhere.clone())))),
    );
    let general = screen.general.as_ref().unwrap();
    assert_eq!(general.name.value(), "Notes Pro", "the typed draft is kept");
    assert_eq!(general.about.value(), "Terminal-first notes.");
    assert_eq!(general.revision, 5);
    assert_eq!(
        general.error.as_deref(),
        Some(
            "Changed elsewhere: description, platforms. Enter saves your edit over it; Esc keeps the saved one."
        ),
        "the owner's own name edit and the roadmap choice the form has no switch for are not named"
    );

    let ProjectReply::Change(sent) = screen.key(key(KeyCode::Enter)) else {
        panic!("Enter saves the edit over the named fields");
    };
    let SetupAction::Update {
        expected_revision,
        setup,
        ..
    } = &*sent
    else {
        panic!("the General form sends a setup update");
    };
    assert_eq!(*expected_revision, 5);
    assert_eq!(setup.about, "Terminal-first notes.");
    assert!(
        setup.research.contains(&ResearchKind::RoadmapPlanning),
        "the roadmap choice saved elsewhere is kept"
    );
    screen.apply(&change, stale());
    let read = screen
        .next_query(30)
        .expect("a second stale refusal reads the setup again");
    elsewhere.revision = 6;
    elsewhere.tech_stack.push("tauri".into());
    screen.apply(&read, Ok(SetupView::Setup(Some(Box::new(elsewhere)))));
    assert_eq!(
        screen.general.as_ref().unwrap().error.as_deref(),
        Some(
            "Changed elsewhere: tech stack. Enter saves your edit over it; Esc keeps the saved one."
        ),
        "fields already named are compared from the setup the owner was last shown"
    );
}

/// What the remove ask shows: the engine's preview of a project whose folders are all inside
/// the projects home, or only its dashboard choice when one is outside.
fn removal(can_delete: bool) -> crate::team::cleanup::RemovalPreview {
    crate::team::cleanup::RemovalPreview {
        name: "Gone app".into(),
        folders: vec!["/Users/owner/CrazyAgent/gone-app".into()],
        can_delete,
        blocked: None,
        running: vec!["Builder 2".into()],
        open_change_sets: 1,
        losses: vec![crate::team::cleanup::AgentLoss {
            id: 3,
            label: "Tester 3".into(),
            code: "cleanup_worktree_dirty".into(),
        }],
    }
}

/// The remove ask reads what removing would do and preselects Remove from dashboard (keep
/// files); Delete everything shows only when every folder is inside the projects home, lists
/// what would be lost, and sends only once the project's exact name is typed (DEC-118).
#[test]
fn remove_preselects_the_dashboard_choice_and_deletes_only_after_the_exact_name() {
    let project = ProjectId::new("gone").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Gone app".into(), None);
    screen.open(Tab::Settings);
    screen.key(key(KeyCode::Char('x')));
    assert_eq!(screen.remove_choice, RemoveChoice::Dashboard);
    let read = screen.next_query(30);
    assert_eq!(
        read,
        Some(SetupQuery::Removal {
            project: project.clone()
        }),
        "the ask reads what removing would do first"
    );
    screen.apply(
        read.as_ref().unwrap(),
        Ok(SetupView::Removal(Box::new(removal(true)))),
    );
    let shown = text(&project_screen(&mut screen, 90, 30, &Look::plain()).rows);
    for words in [
        "Remove from dashboard (keep files)",
        "Delete everything",
        "Builder 2",
        "Tester 3",
        "1 change set",
    ] {
        assert!(shown.contains(words), "{words:?} missing: {shown}");
    }
    assert_eq!(screen.key(key(KeyCode::Down)), ProjectReply::Handled);
    assert_eq!(screen.remove_choice, RemoveChoice::Everything);
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Handled,
        "nothing is sent before the name is typed"
    );
    for letter in "Gone app ".chars() {
        assert_eq!(
            screen.key(key(KeyCode::Char(letter))),
            ProjectReply::Handled
        );
    }
    assert!(
        screen.confirm_remove,
        "typing n or y is text, not an answer"
    );
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Handled,
        "a trailing space is not the name"
    );
    screen.key(key(KeyCode::Backspace));
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(SetupAction::Delete {
            project_id: project.clone(),
            name: "Gone app".into(),
        }))
    );
    assert!(!screen.confirm_remove);

    screen.key(key(KeyCode::Char('x')));
    assert_eq!(screen.remove_choice, RemoveChoice::Dashboard);
    assert!(screen.remove_typed.is_empty());
    let read = screen.next_query(60).unwrap();
    screen.apply(&read, Ok(SetupView::Removal(Box::new(removal(false)))));
    let shown = text(&project_screen(&mut screen, 90, 30, &Look::plain()).rows);
    assert!(
        shown.contains("Remove from dashboard (keep files)"),
        "{shown}"
    );
    assert!(
        !shown.contains("Delete everything"),
        "a project outside the projects home offers no delete: {shown}"
    );
    screen.key(key(KeyCode::Down));
    assert_eq!(screen.remove_choice, RemoveChoice::Dashboard);
    assert_eq!(
        screen.key(key(KeyCode::Char('y'))),
        ProjectReply::Change(Box::new(SetupAction::Remove {
            project_id: project.clone(),
        }))
    );
}

/// When the engine refuses Delete everything, the remove ask offers only Remove from
/// dashboard and says why in the engine's own sentence, which names the other project.
#[test]
fn the_remove_ask_says_why_delete_everything_is_not_offered() {
    let project = ProjectId::new("gone").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Gone app".into(), None);
    screen.open(Tab::Settings);
    screen.key(key(KeyCode::Char('x')));
    let read = screen.next_query(30).unwrap();
    let mut preview = removal(false);
    preview.blocked = Some(
        "Service A uses a folder of this project, so deleting it would delete that project's files too. Remove it from the dashboard instead."
            .into(),
    );
    screen.apply(&read, Ok(SetupView::Removal(Box::new(preview))));
    let shown = text(&project_screen(&mut screen, 90, 30, &Look::plain()).rows);
    assert!(shown.contains("Service A uses a folder"), "{shown}");
    assert!(!shown.contains("Type the project name"), "{shown}");
    screen.key(key(KeyCode::Down));
    assert_eq!(screen.remove_choice, RemoveChoice::Dashboard);
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
        ["setup", "jobs", "overview", "context", "memories"]
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

#[test]
fn the_read_only_folders_box_saves_colon_separated_folders_and_shows_refusals() {
    let look = Look::plain();
    let project = ProjectId::new("folders").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('f'))), ProjectReply::Handled);
    assert!(screen.folders.is_none());
    assert_eq!(
        screen.problem.as_deref(),
        Some("Still loading the read-only folders. Try again.")
    );
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Folders   Loading...   f Edit read-only folders"),
        "{shown}"
    );

    let mut read = None;
    for now in 20..26 {
        let query = screen.next_query(now).unwrap();
        if matches!(query, SetupQuery::ReadPaths { .. }) {
            read = Some(query);
            break;
        }
        screen.apply(&query, Err("trusted_connection_unavailable".into()));
    }
    let read = read.expect("the Settings tab reads the read-only folders");
    assert_eq!(
        read,
        SetupQuery::ReadPaths {
            project: project.clone()
        }
    );
    screen.apply(
        &read,
        Ok(SetupView::ReadPaths(crate::project_setup::ReadPaths {
            paths: vec!["/Users/owner/shared".into()],
            version: 3,
        })),
    );
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Folders   /Users/owner/shared   f Edit read-only folders"),
        "{shown}"
    );

    assert_eq!(screen.key(key(KeyCode::Char('f'))), ProjectReply::Handled);
    assert_eq!(
        screen.folders.as_ref().unwrap().input.value(),
        "/Users/owner/shared"
    );
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("Read-only folders"));
    assert_eq!(drawn.help, [("Enter", "Save folders"), ("Esc", "Cancel")]);
    assert!(drawn.cursor.is_some());
    assert_eq!(screen.paste(" : relative : "), ProjectReply::Handled);
    let save = SetupAction::SetReadPaths {
        project_id: project.clone(),
        version: 3,
        paths: vec!["/Users/owner/shared".into(), "relative".into()],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save.clone()))
    );
    assert!(screen.folders.as_ref().unwrap().busy);
    let change = SetupQuery::Change {
        action: save,
        generation: 2,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "read_path_not_absolute".into(),
        })),
    );
    let folders = screen.folders.as_ref().unwrap();
    assert!(!folders.busy);
    assert_eq!(
        folders.error.as_deref(),
        Some("Type the full path of each folder, starting with /.")
    );
    assert_eq!(
        screen.read_paths, None,
        "a refused change reads the folders again"
    );

    screen.folders.as_mut().unwrap().input.set("");
    let clear = SetupAction::SetReadPaths {
        project_id: project.clone(),
        version: 3,
        paths: vec![],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(clear.clone()))
    );
    screen.apply(
        &SetupQuery::Change {
            action: clear,
            generation: 3,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(screen.folders.is_none());
    assert_eq!(screen.read_paths, None);

    screen.read_paths = Some(crate::project_setup::ReadPaths {
        paths: vec![],
        version: 4,
    });
    screen.key(key(KeyCode::Char('f')));
    screen.paste(&format!("/{}", "d".repeat(1100)));
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.folders.as_ref().unwrap().error.as_deref(),
        Some("Share at most 8 folders, each one plain path of up to 1024 characters.")
    );
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.folders.is_none());
}

/// Answers the screen's reads as unavailable until it asks for the read-only folders.
/// Inputs: the screen and the time of the first read. Output: the folders read.
fn access_view(access: AgentAccess, version: u64) -> PolicyView {
    PolicyView {
        access,
        network: crate::sandbox::policy::AgentNetwork::default(),
        levels: AgentAccess::ALL
            .into_iter()
            .map(|level| LevelView {
                access: level,
                label: level.label().to_owned(),
                unavailable: None,
            })
            .collect(),
        networks: crate::sandbox::policy::AgentNetwork::ALL
            .into_iter()
            .map(|network| crate::sandbox::policy::NetworkView {
                network,
                label: network.label().to_owned(),
            })
            .collect(),
        toolchains: Vec::new(),
        ide_git_hooks: false,
        tool_cache: Default::default(),
        tool_caches: Vec::new(),
        ide_agent_files: Default::default(),
        version,
        apply_changes: Default::default(),
        apply_choices: Vec::new(),
        grants: Vec::new(),
        temp_folder: Default::default(),
        grant_log: Vec::new(),
    }
}

fn next_access_read(screen: &mut ProjectScreen, from: i64) -> SetupQuery {
    for now in from..from + 6 {
        let query = screen.next_query(now).expect("a read is due");
        if matches!(query, SetupQuery::AgentPolicy { .. }) {
            return query;
        }
        screen.apply(&query, Err("trusted_connection_unavailable".into()));
    }
    panic!("the Settings tab reads the agent access");
}

/// An Agent access save names the version the box opened at; refused as stale it keeps the
/// owner's level, reads the saved policy again and shows it, and Enter then saves the
/// owner's level at the version just read (Q35).
#[test]
fn a_stale_access_save_keeps_the_level_and_enter_saves_it_over_the_re_read() {
    let project = ProjectId::new("access-stale").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    let read = next_access_read(&mut screen, 20);
    screen.apply(
        &read,
        Ok(SetupView::AgentPolicy(access_view(
            AgentAccess::OwnBranch,
            3,
        ))),
    );
    screen.key(key(KeyCode::Char('c')));
    assert_eq!(screen.key(key(KeyCode::Left)), ProjectReply::Handled);
    let save_at = |version: u64| SetupAction::SetAgentPolicy {
        project_id: project.clone(),
        version,
        policy: AgentPolicy {
            access: AgentAccess::Sandboxed,
            toolchains: Vec::new(),
            ide_git_hooks: false,
            tool_cache: Default::default(),
            ide_agent_files: Default::default(),
            network: crate::sandbox::policy::AgentNetwork::default(),
            ..Default::default()
        },
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save_at(3)))
    );
    screen.apply(
        &SetupQuery::Change {
            action: save_at(3),
            generation: 2,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "stale_revision".into(),
        })),
    );
    let access = screen.access.as_ref().unwrap();
    assert!(!access.busy);
    assert_eq!(
        access.access,
        AgentAccess::Sandboxed,
        "the owner's level is kept"
    );
    assert_eq!(
        access.error.as_deref(),
        Some(
            "This project changed since you opened the form. Your edit is kept while the saved one is read."
        )
    );
    assert_eq!(screen.agent_policy, None);

    let read = next_access_read(&mut screen, 40);
    screen.apply(
        &read,
        Ok(SetupView::AgentPolicy(access_view(AgentAccess::Full, 4))),
    );
    let access = screen.access.as_ref().unwrap();
    assert_eq!(access.access, AgentAccess::Sandboxed);
    assert_eq!(
        access.error.as_deref(),
        Some(
            "Saved now: Full access · no toolchains. Enter saves your edit over it; Esc keeps the saved one."
        )
    );
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save_at(4))),
        "Enter keeps the owner's level over the saved one"
    );
}

fn next_folders_read(screen: &mut ProjectScreen, from: i64) -> SetupQuery {
    for now in from..from + 6 {
        let query = screen.next_query(now).expect("a read is due");
        if matches!(query, SetupQuery::ReadPaths { .. }) {
            return query;
        }
        screen.apply(&query, Err("trusted_connection_unavailable".into()));
    }
    panic!("the Settings tab reads the read-only folders");
}

#[test]
fn a_stale_folders_save_keeps_the_list_and_enter_saves_it_over_the_re_read() {
    let project = ProjectId::new("folders-stale").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    let read = next_folders_read(&mut screen, 20);
    screen.apply(
        &read,
        Ok(SetupView::ReadPaths(crate::project_setup::ReadPaths {
            paths: vec!["/Users/owner/shared".into()],
            version: 3,
        })),
    );
    screen.key(key(KeyCode::Char('f')));
    assert_eq!(screen.paste(":/Users/owner/notes"), ProjectReply::Handled);
    let save_at = |version: u64| SetupAction::SetReadPaths {
        project_id: project.clone(),
        version,
        paths: vec!["/Users/owner/shared".into(), "/Users/owner/notes".into()],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save_at(3)))
    );
    let change = SetupQuery::Change {
        action: save_at(3),
        generation: 2,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "stale_revision".into(),
        })),
    );
    let folders = screen.folders.as_ref().unwrap();
    assert!(!folders.busy);
    assert_eq!(
        folders.input.value(),
        "/Users/owner/shared:/Users/owner/notes"
    );
    assert_eq!(
        folders.error.as_deref(),
        Some(
            "This project changed since you opened the form. Your edit is kept while the saved one is read."
        )
    );
    assert_eq!(screen.read_paths, None);

    let read = next_folders_read(&mut screen, 40);
    screen.apply(
        &read,
        Ok(SetupView::ReadPaths(crate::project_setup::ReadPaths {
            paths: vec!["/Users/owner/other".into()],
            version: 4,
        })),
    );
    let folders = screen.folders.as_ref().unwrap();
    assert_eq!(
        folders.input.value(),
        "/Users/owner/shared:/Users/owner/notes",
        "the typed list is kept"
    );
    assert_eq!(
        folders.error.as_deref(),
        Some(
            "Saved now: /Users/owner/other. Enter saves your edit over it; Esc keeps the saved one."
        )
    );
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save_at(4))),
        "Enter keeps the owner's list over the saved one"
    );

    screen.apply(
        &SetupQuery::Change {
            action: save_at(4),
            generation: 3,
        },
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "read_path_not_absolute".into(),
        })),
    );
    let read = next_folders_read(&mut screen, 60);
    screen.apply(
        &read,
        Ok(SetupView::ReadPaths(crate::project_setup::ReadPaths {
            paths: vec![],
            version: 5,
        })),
    );
    assert_eq!(
        screen.folders.as_ref().unwrap().error.as_deref(),
        Some("Type the full path of each folder, starting with /."),
        "a refusal that is not stale keeps the version the box opened at"
    );
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save_at(4)))
    );

    screen.apply(
        &SetupQuery::Change {
            action: save_at(4),
            generation: 4,
        },
        Err("stale_revision".into()),
    );
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    let read = next_folders_read(&mut screen, 80);
    screen.apply(
        &read,
        Ok(SetupView::ReadPaths(crate::project_setup::ReadPaths {
            paths: vec![],
            version: 6,
        })),
    );
    assert!(screen.folders.is_none(), "Esc keeps the saved one");
    screen.key(key(KeyCode::Char('f')));
    let folders = screen.folders.as_ref().unwrap();
    assert_eq!(folders.input.value(), "");
    assert_eq!(
        folders.error, None,
        "a box opened afresh carries no stale note"
    );
}

/// Once the research jobs and a context with no roadmap document are read, `p` on the
/// Settings tab plans a roadmap at once and `k` opens the hidden folders box, so neither
/// action takes the other's key.
#[test]
fn settings_plans_a_roadmap_with_p_and_edits_hidden_folders_with_k() {
    let look = Look::plain();
    let project = ProjectId::new("hidden").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    screen.jobs = Some(Vec::new());
    screen.hidden_paths = Some(vec!["/Users/owner/secret".into()]);
    screen.context = Some(crate::project_setup::ProjectContext {
        project_id: project.clone(),
        folder: "/Users/owner/hidden".into(),
        items: vec![],
    });
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Roadmap   Not planned   p Plan roadmap now"),
        "{shown}"
    );
    assert!(
        shown.contains("Hidden    /Users/owner/secret   k Edit hidden folders"),
        "{shown}"
    );

    let ProjectReply::Change(action) = screen.key(key(KeyCode::Char('p'))) else {
        panic!("p asks the engine to plan");
    };
    assert_eq!(
        *action,
        SetupAction::PlanRoadmap {
            project_id: project.clone(),
        }
    );
    assert!(
        screen.hidden.is_none(),
        "p never opens the hidden folders box"
    );
    assert_eq!(screen.key(key(KeyCode::Char('k'))), ProjectReply::Handled);
    assert_eq!(
        screen.hidden.as_ref().unwrap().input.value(),
        "/Users/owner/secret"
    );
}

#[test]
fn the_hidden_folders_box_saves_colon_separated_folders_and_shows_refusals() {
    let look = Look::plain();
    let project = ProjectId::new("hidden").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    screen.read_paths = Some(crate::project_setup::ReadPaths {
        paths: vec![],
        version: 0,
    });
    assert_eq!(screen.key(key(KeyCode::Char('k'))), ProjectReply::Handled);
    assert!(screen.hidden.is_none());
    assert_eq!(
        screen.problem.as_deref(),
        Some("Still loading the hidden folders. Try again.")
    );
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Hidden    Loading...   k Edit hidden folders"),
        "{shown}"
    );

    let mut read = None;
    for now in 20..26 {
        let query = screen.next_query(now).unwrap();
        if matches!(query, SetupQuery::HiddenPaths { .. }) {
            read = Some(query);
            break;
        }
        screen.apply(&query, Err("trusted_connection_unavailable".into()));
    }
    let read = read.expect("the Settings tab reads the hidden folders");
    assert_eq!(
        read,
        SetupQuery::HiddenPaths {
            project: project.clone()
        }
    );
    screen.apply(
        &read,
        Ok(SetupView::HiddenPaths(vec!["/Users/owner/secret".into()])),
    );
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Hidden    /Users/owner/secret   k Edit hidden folders"),
        "{shown}"
    );

    assert_eq!(screen.key(key(KeyCode::Char('k'))), ProjectReply::Handled);
    assert_eq!(
        screen.hidden.as_ref().unwrap().input.value(),
        "/Users/owner/secret"
    );
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(text(&drawn.rows).contains("Hidden folders"));
    assert_eq!(
        drawn.help,
        [("Enter", "Save hidden folders"), ("Esc", "Cancel")]
    );
    assert!(drawn.cursor.is_some());
    assert_eq!(
        screen.paste(" : /Users/owner/project/vendor : "),
        ProjectReply::Handled
    );
    let save = SetupAction::SetHiddenPaths {
        project_id: project.clone(),
        paths: vec![
            "/Users/owner/secret".into(),
            "/Users/owner/project/vendor".into(),
        ],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(save.clone()))
    );
    assert!(screen.hidden.as_ref().unwrap().busy);
    let change = SetupQuery::Change {
        action: save,
        generation: 2,
    };
    screen.apply(
        &change,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "hidden_path_outside_project".into(),
        })),
    );
    let hidden = screen.hidden.as_ref().unwrap();
    assert!(!hidden.busy);
    assert_eq!(
        hidden.error.as_deref(),
        Some("A hidden folder must sit inside the project. Pick a folder within it.")
    );
    assert_eq!(
        screen.hidden_paths, None,
        "a refused change reads the hidden folders again"
    );

    screen.hidden.as_mut().unwrap().input.set("");
    let clear = SetupAction::SetHiddenPaths {
        project_id: project.clone(),
        paths: vec![],
    };
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Change(Box::new(clear.clone()))
    );
    screen.apply(
        &SetupQuery::Change {
            action: clear,
            generation: 3,
        },
        Ok(SetupView::Changed(OwnerResult::Applied)),
    );
    assert!(screen.hidden.is_none());
    assert_eq!(screen.hidden_paths, None);

    screen.hidden_paths = Some(vec![]);
    screen.key(key(KeyCode::Char('k')));
    screen.paste(&format!("/{}", "d".repeat(1100)));
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.hidden.as_ref().unwrap().error.as_deref(),
        Some("Each folder must be one plain path of up to 1024 characters.")
    );
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(screen.hidden.is_none());
}

fn console_screen() -> ProjectScreen {
    use super::project::{Message, Speaker};
    use crate::project_setup::{ContextItem, ProjectContext};
    use crate::team::{GraphNode, NodeKind, TeamGraph};
    let project = ProjectId::new("notes-cli").unwrap();
    let mut screen = loaded(&project);
    screen.setup.as_mut().expect("loaded").team.master = "rcli".into();
    screen.pool = Some(PoolInfo {
        id: CredentialRef::new("pool-main").unwrap(),
        name: "Main".into(),
        accounts: 1,
        assigned: true,
    });
    let said = |speaker, text: &str| Message {
        speaker,
        time: None,
        text: text.into(),
        error: false,
    };
    screen.transcript = Some(Transcript {
        harness: "RCLI".into(),
        account: None,
        tools: true,
        open: true,
        status: MasterStatus::Ready,
        messages: vec![
            said(Speaker::You, "Ask @page to fix the header"),
            said(Speaker::Master, "I handed it to Ada."),
            said(Speaker::Task, "Task 8 is still with a sub-agent."),
            said(Speaker::Notice, "Its edits wait in Changes."),
        ],
    });
    let node = |id: &str, kind, label: &str| GraphNode {
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
    };
    screen.team = crate::tui::ctx::Load::Ready(Box::new(TeamGraph {
        project_id: project.clone(),
        nodes: vec![
            node("master", NodeKind::Master, "Master"),
            node("team-1", NodeKind::Lead, "Ada (@page)"),
        ],
        edges: vec![],
        messages: Vec::new(),
        overlaps: Vec::new(),
        relations: Vec::new(),
        master_live_terminal: true,
    }));
    screen.context = Some(ProjectContext {
        project_id: project,
        folder: "/private/tmp/ca-notes-cli".into(),
        items: vec![ContextItem {
            path: "docs/brief.md".into(),
            kind: ContextKind::Document,
            bytes: 10,
            linked: false,
        }],
    });
    screen.open(Tab::Conversation);
    assert!(screen.typing);
    screen
}

fn type_text(screen: &mut ProjectScreen, text: &str) {
    for character in text.chars() {
        assert_eq!(
            screen.key(key(KeyCode::Char(character))),
            ProjectReply::Handled
        );
    }
}

#[test]
fn a_running_master_turn_shows_what_it_last_did_and_ctrl_o_opens_the_whole_list() {
    use crate::master_session::MasterActivityLine;
    let mut screen = console_screen();
    let now: i64 = 3_600_000;
    let texts = [
        "Searching the code",
        "Reading README.md",
        "Running a command",
        "Editing src/app/layout.tsx",
        "Updating its plan",
        "Writing a reply",
        "Reading src/app/page.tsx",
    ];
    let lines: Vec<MasterActivityLine> = texts
        .iter()
        .zip(0u64..)
        .map(|(text, step)| MasterActivityLine {
            at_ms: 3_600_000 - 4_000 - (6 - step) * 10_000,
            text: (*text).into(),
        })
        .collect();
    let thinking = |activity: &[MasterActivityLine]| MasterStatus::Thinking {
        since_ms: Some(now - 6 * 60_000),
        activity: activity.to_vec(),
    };
    let look = Look::plain();
    let control_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let draw = |screen: &mut ProjectScreen| {
        let drawn = project_screen(screen, 90, 40, &look);
        let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
        (rows, drawn.help)
    };
    let under = |rows: &[String], count: usize| -> Vec<String> {
        let at = rows
            .iter()
            .rposition(|row| row.contains("Thinking..."))
            .unwrap_or_else(|| panic!("{}", rows.join("\n")));
        rows[at + 1..at + 1 + count]
            .iter()
            .map(|row| row.trim().to_owned())
            .collect()
    };
    screen.now_ms = now;

    screen.transcript.as_mut().expect("console").status = thinking(&lines[2..]);
    let (rows, help) = draw(&mut screen);
    let shown = rows.join("\n");
    assert!(
        shown.contains("Thinking...  6 min \u{b7} Reading src/app/page.tsx, 4 s ago"),
        "{shown}"
    );
    assert!(!screen.more_activity());
    assert!(!help.iter().any(|(key, _)| *key == "Ctrl-O"), "{help:?}");
    screen.key(control_o);
    assert!(!screen.activity_open, "five lines have no longer list");

    screen.transcript.as_mut().expect("console").status = thinking(&lines);
    let (rows, help) = draw(&mut screen);
    let shown = rows.join("\n");
    assert_eq!(
        under(&rows, 5),
        [
            "44 s ago  Running a command",
            "34 s ago  Editing src/app/layout.tsx",
            "24 s ago  Updating its plan",
            "14 s ago  Writing a reply",
            "4 s ago  Reading src/app/page.tsx",
        ],
        "{shown}"
    );
    assert!(!shown.contains("Searching the code"), "{shown}");
    assert!(!shown.contains("Reading README.md"), "{shown}");
    assert!(help.contains(&("Ctrl-O", "All activity")), "{help:?}");

    assert_eq!(screen.key(control_o), ProjectReply::Handled);
    assert!(screen.activity_open);
    assert_eq!(screen.input.value(), "", "the key types nothing");
    let (rows, help) = draw(&mut screen);
    let listed = under(&rows, 7);
    assert_eq!(listed[0], "1 min ago  Searching the code");
    assert_eq!(listed[1], "54 s ago  Reading README.md");
    assert_eq!(listed[6], "4 s ago  Reading src/app/page.tsx");
    assert!(help.contains(&("Ctrl-O", "Less activity")), "{help:?}");

    screen.now_ms = now + 9 * 60_000;
    let (rows, _) = draw(&mut screen);
    let shown = rows.join("\n");
    assert!(
        shown.contains("Thinking...  15 min \u{b7} no new activity for 9 min"),
        "{shown}"
    );

    screen.transcript.as_mut().expect("console").status = MasterStatus::Replied;
    let (rows, help) = draw(&mut screen);
    let shown = rows.join("\n");
    assert!(!shown.contains("Thinking"), "{shown}");
    assert!(!shown.contains("src/app/page.tsx"), "{shown}");
    assert!(!help.iter().any(|(key, _)| *key == "Ctrl-O"), "{help:?}");
    assert!(!screen.more_activity());
}

#[test]
fn an_activity_line_wider_than_the_screen_is_cut_to_its_width() {
    use crate::master_session::MasterActivityLine;
    let mut screen = console_screen();
    let long = format!("Reading {}", "deep/".repeat(36));
    screen.now_ms = 60_000;
    screen.transcript.as_mut().expect("console").status = MasterStatus::Thinking {
        since_ms: Some(0),
        activity: vec![MasterActivityLine {
            at_ms: 56_000,
            text: long,
        }],
    };
    let drawn = project_screen(&mut screen, 60, 30, &Look::plain());
    let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
    let shown = rows.join("\n");
    let wide: Vec<&String> = rows.iter().filter(|row| row.contains("deep/")).collect();
    assert_eq!(wide.len(), 2, "{shown}");
    for row in wide {
        assert!(row.chars().count() <= 60, "{shown}");
        assert!(row.trim_end().ends_with('\u{2026}'), "{shown}");
    }
}

#[test]
fn the_master_console_draws_the_transcript_and_prompt_like_a_terminal() {
    let mut screen = console_screen();
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
    let shown = rows.join("\n");
    assert!(
        rows.iter()
            .any(|row| row.trim() == "> Ask @page to fix the header"),
        "{shown}"
    );
    assert!(
        rows.iter().any(|row| row.trim() == "I handed it to Ada."),
        "{shown}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim() == "\u{2022} Task 8 is still with a sub-agent."),
        "{shown}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim() == "Its edits wait in Changes."),
        "{shown}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.trim() == "You" || row.trim() == "RCLI"),
        "no chat headers:\n{shown}"
    );
    let prompt = rows
        .iter()
        .rposition(|row| row.trim() == "> Message the master, / for commands, @ to tag")
        .expect("one prompt line");
    assert!(rows[prompt - 1].trim().starts_with('\u{2500}'), "{shown}");
    assert_eq!(drawn.cursor, Some((4, u16::try_from(prompt).unwrap())));
    let owner = drawn
        .rows
        .iter()
        .find(|row| row.text().trim().starts_with("> Ask"))
        .unwrap();
    let tag = owner
        .0
        .iter()
        .find(|span| span.content == "@page")
        .expect("the tag is its own span");
    assert_eq!(tag.style, look.accent);
    let notice = drawn
        .rows
        .iter()
        .find(|row| row.text().trim().starts_with("Its edits"))
        .unwrap();
    assert!(
        notice
            .0
            .iter()
            .filter(|span| !span.content.trim().is_empty())
            .all(|span| span.style == look.muted),
        "engine lines are dimmed"
    );

    type_text(&mut screen, "first");
    for _ in 0..9 {
        screen.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        type_text(&mut screen, "x");
    }
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
    let rule = rows
        .iter()
        .rposition(|row| row.trim().starts_with('\u{2500}'))
        .unwrap();
    let prompt: Vec<&str> = rows[rule + 1..]
        .iter()
        .map(|row| row.trim_end())
        .take_while(|row| !row.is_empty())
        .collect();
    assert_eq!(
        prompt,
        ["    x"; 8],
        "the prompt grows to at most 8 lines and follows the cursor line:\n{}",
        rows.join("\n")
    );
    assert_eq!(drawn.cursor, Some((5, u16::try_from(rule + 8).unwrap())));
}

#[test]
fn the_console_sends_on_enter_breaks_lines_recalls_history_and_clears_on_escape() {
    let mut screen = console_screen();
    type_text(&mut screen, "hello");
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Send("hello".into())
    );
    assert_eq!(screen.input.value(), "");
    type_text(&mut screen, "a");
    assert_eq!(screen.key(shifted(KeyCode::Enter)), ProjectReply::Handled);
    screen.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    type_text(&mut screen, "b");
    assert_eq!(screen.input.value(), "a\n\nb");
    assert_eq!(screen.key(key(KeyCode::Up)), ProjectReply::Handled);
    assert_eq!(
        screen.input.value(),
        "a\n\nb",
        "Up recalls only from an empty prompt"
    );
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "");
    assert!(screen.typing, "Esc clears the prompt first");
    screen.key(key(KeyCode::Up));
    assert_eq!(screen.input.value(), "hello");
    screen.key(key(KeyCode::Down));
    assert_eq!(screen.input.value(), "");
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(!screen.typing, "Esc on an empty prompt leaves it");
}

#[test]
fn a_path_message_and_a_draft_ending_in_a_slash_word_are_sent_not_run() {
    use super::project::Picks;
    let mut screen = console_screen();
    type_text(&mut screen, "/etc/hosts has a bad entry, fix it");
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Send("/etc/hosts has a bad entry, fix it".into())
    );
    assert!(screen.console_note.is_empty(), "{:?}", screen.console_note);
    type_text(&mut screen, "/tmp is full");
    screen.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    type_text(&mut screen, "clean it");
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Send("/tmp is full\nclean it".into()),
        "a draft starting with a /word is a message"
    );
    type_text(&mut screen, "/stop now");
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Send("/stop now".into())
    );
    type_text(&mut screen, "/deploy");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "");
    screen.key(key(KeyCode::Up));
    assert_eq!(
        screen.input.value(),
        "/deploy",
        "a command's text stays in history"
    );
    screen.key(key(KeyCode::Esc));
    screen.console_note.clear();

    for last in ["/help", "/st"] {
        type_text(&mut screen, "Steps:");
        screen.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        type_text(&mut screen, last);
        assert!(
            matches!(screen.picks(), Picks::None),
            "{last}: a later line opens no menu"
        );
        let draft = format!("Steps:\n{last}");
        assert_eq!(screen.input.value(), draft);
        assert_eq!(
            screen.key(key(KeyCode::Enter)),
            ProjectReply::Send(draft.clone()),
            "{last}: the whole draft is sent, nothing runs"
        );
        assert!(screen.console_note.is_empty(), "{:?}", screen.console_note);
    }
}

#[test]
fn slash_opens_the_command_menu_and_runs_the_chosen_command() {
    use super::project::Picks;
    let mut screen = console_screen();
    type_text(&mut screen, "/c");
    let names = |screen: &ProjectScreen| match screen.picks() {
        Picks::Commands(commands) => commands
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(names(&screen), ["clear", "changes"]);
    let drawn = project_screen(&mut screen, 80, 24, &Look::plain());
    let shown = text(&drawn.rows);
    assert!(
        shown.contains("\u{25b8} /clear    Clear this view; nothing is deleted"),
        "{shown}"
    );
    assert!(
        shown.contains("  /changes  Open the Changes tab"),
        "{shown}"
    );
    assert!(drawn.help.contains(&("Enter", "Run")), "{:?}", drawn.help);
    assert_eq!(screen.key(key(KeyCode::Down)), ProjectReply::Handled);
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "/changes");
    assert!(
        screen.typing,
        "Tab completes in the menu instead of leaving the prompt"
    );
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(screen.tab, Tab::Changes);

    let mut screen = console_screen();
    for (typed, reply) in [
        ("/stop", ProjectReply::StopTurn),
        ("/usage", ProjectReply::Usage),
        ("/process", ProjectReply::Process),
    ] {
        type_text(&mut screen, typed);
        assert_eq!(screen.key(key(KeyCode::Enter)), reply, "{typed}");
        assert_eq!(screen.input.value(), "");
    }
    type_text(&mut screen, "/deploy");
    assert_eq!(names(&screen), Vec::<&str>::new());
    let shown = text(&project_screen(&mut screen, 80, 24, &Look::plain()).rows);
    assert!(shown.contains("  Unknown command"), "{shown}");
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert_eq!(
        screen.console_note,
        ["Unknown command /deploy. Type /help for the commands."]
    );
    type_text(&mut screen, "/help");
    screen.key(key(KeyCode::Enter));
    assert_eq!(screen.console_note.len(), 8);
    assert_eq!(screen.console_note[0], "/help     Show the commands");
    let shown = text(&project_screen(&mut screen, 80, 24, &Look::plain()).rows);
    assert!(
        shown.contains("/stop     Stop the master's turn"),
        "{shown}"
    );
    type_text(&mut screen, "/clear");
    screen.key(key(KeyCode::Enter));
    let shown = text(&project_screen(&mut screen, 80, 24, &Look::plain()).rows);
    assert!(
        !shown.contains("Ask @page"),
        "/clear empties the view:\n{shown}"
    );
    assert_eq!(
        screen.transcript.as_ref().unwrap().messages.len(),
        4,
        "nothing is deleted"
    );
}

#[test]
fn at_completes_agent_handles_and_project_files_and_inserts_the_tag() {
    use super::project::Picks;
    let mut screen = console_screen();
    type_text(&mut screen, "ask @");
    let drawn = project_screen(&mut screen, 80, 24, &Look::plain());
    let shown = text(&drawn.rows);
    for tag in ["\u{25b8} @master", "  @page", "  @docs/brief.md"] {
        assert!(shown.contains(tag), "{tag}:\n{shown}");
    }
    type_text(&mut screen, "p");
    assert_eq!(
        screen.picks(),
        Picks::Tags {
            typed: 2,
            tags: vec!["@page".into()]
        }
    );
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert_eq!(screen.input.value(), "ask @page ");
    type_text(&mut screen, "to read @do");
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Handled,
        "Enter takes the tag, not the send"
    );
    assert_eq!(screen.input.value(), "ask @page to read @docs/brief.md ");
    assert_eq!(screen.picks(), Picks::None);
    assert_eq!(screen.files_query(), None);
    type_text(&mut screen, "@pla");
    assert_eq!(screen.files_query().as_deref(), Some("pla"));
    assert_eq!(screen.picks(), Picks::None);
    screen.files = Some(("pla".into(), vec!["src/planner.rs".into()]));
    assert_eq!(
        screen.picks(),
        Picks::Tags {
            typed: 4,
            tags: vec!["@src/planner.rs".into()]
        },
        "the engine's file list completes the word"
    );
    assert_eq!(screen.key(key(KeyCode::Tab)), ProjectReply::Handled);
    assert_eq!(
        screen.key(key(KeyCode::Enter)),
        ProjectReply::Send("ask @page to read @docs/brief.md @src/planner.rs".into())
    );
}

#[test]
fn a_project_sets_its_own_account_order_or_follows_its_pool() {
    use crate::scheduling::{Schedule, ScopedStrategy, Strategy};
    let look = Look::plain();
    let project = ProjectId::new("order").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('u'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("Still loading the account order. Try again.")
    );
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Accounts  Loading...   u Change order"),
        "{shown}"
    );
    let mut schedule = Schedule {
        strategy: Strategy::ManualRank,
        ..Schedule::default()
    };
    screen.schedule = Some(schedule.clone());
    assert_eq!(
        screen.account_order().as_deref(),
        Some("Default (Manual rank)")
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('u'))),
        ProjectReply::AccountOrder(Some(Strategy::UseItOrLoseIt))
    );
    schedule.projects.push(ScopedStrategy {
        id: CredentialRef::new("order").unwrap(),
        strategy: Strategy::Balanced,
    });
    screen.schedule = Some(schedule);
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("Accounts  Balanced (this project)   u Change order"),
        "{shown}"
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('u'))),
        ProjectReply::AccountOrder(None)
    );
}

#[test]
fn v_on_the_context_tab_asks_for_the_selected_file_in_the_editor_from_the_folder_its_path_starts_in()
 {
    use super::project::OpenRequest;
    use crate::project_setup::{ContextItem, ProjectContext};
    let project = ProjectId::new("notes-cli").unwrap();
    let mut screen = loaded(&project);
    assert_eq!(
        screen.project_folder(),
        Some(std::path::PathBuf::from("/private/tmp/ca-notes-cli"))
    );
    screen.open(Tab::Context);
    assert_eq!(
        screen.key(key(KeyCode::Char('v'))),
        ProjectReply::Handled,
        "nothing is listed yet"
    );
    screen.context = Some(ProjectContext {
        project_id: project.clone(),
        folder: "/private/tmp/ca-notes-cli/.crazyagent".into(),
        items: vec![
            ContextItem {
                path: "context/brief.md".into(),
                kind: ContextKind::Brief,
                bytes: 10,
                linked: false,
            },
            ContextItem {
                path: "context/documents/spec.md".into(),
                kind: ContextKind::Document,
                bytes: 20,
                linked: false,
            },
        ],
    });
    let spec = OpenRequest::Context {
        folder: "/private/tmp/ca-notes-cli/.crazyagent".into(),
        path: "context/documents/spec.md".into(),
    };
    assert_eq!(screen.key(key(KeyCode::Down)), ProjectReply::Handled);
    assert_eq!(
        screen.key(key(KeyCode::Char('v'))),
        ProjectReply::Edit(spec.clone()),
        "context paths start in the state folder the engine listed them from, not the project folder"
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('o'))),
        ProjectReply::Open(spec.clone())
    );
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert!(screen.reading);
    assert_eq!(
        screen.key(key(KeyCode::Char('v'))),
        ProjectReply::Edit(spec),
        "the file being read opens too"
    );
    let look = Look::plain();
    screen.key(key(KeyCode::Esc));
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(drawn.help.contains(&("v", "Edit file")), "{:?}", drawn.help);
    assert!(
        drawn.help.contains(&("o", "Open in app")),
        "{:?}",
        drawn.help
    );

    let mut moved = stored(&project);
    moved.folder = "/private/tmp/ca-notes-cli/.crazyagent".into();
    screen.setup = Some(moved);
    assert_eq!(
        screen.project_folder(),
        Some(std::path::PathBuf::from("/private/tmp/ca-notes-cli")),
        "changed files are the owner's copies in the project folder, never in the state folder"
    );
}

#[test]
fn shift_o_on_the_context_tab_is_in_the_key_help_and_says_why_when_the_ide_cannot_open_the_file() {
    use crate::ide::bridge::reveal::RevealTarget;
    use crate::project_setup::{ContextItem, ProjectContext};
    let project = ProjectId::new("notes-cli").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Context);
    let listed = |folder: &str| ProjectContext {
        project_id: project.clone(),
        folder: folder.into(),
        items: vec![ContextItem {
            path: "context/documents/spec.md".into(),
            kind: ContextKind::Document,
            bytes: 20,
            linked: false,
        }],
    };
    screen.context = Some(listed("/private/tmp/ca-notes-cli/.crazyagent"));
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 120, 36, &look);
    assert!(
        drawn.help.contains(&("O", "Open in IDE")),
        "{:?}",
        drawn.help
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('O'))),
        ProjectReply::Reveal(RevealTarget::Project {
            path: ".crazyagent/context/documents/spec.md".into(),
        })
    );
    assert_eq!(screen.problem, None);

    screen.context = Some(listed("/private/tmp/ca-elsewhere/.crazyagent"));
    assert_eq!(screen.key(key(KeyCode::Char('O'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("This file is outside the project folder, so the IDE can't open it."),
        "a context folder outside the project folder has no IDE path, and the owner is told"
    );

    screen.context = Some(listed("/private/tmp/ca-notes-cli/.crazyagent"));
    screen.setup = None;
    assert_eq!(screen.key(key(KeyCode::Char('O'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("Still loading the project. Try again."),
        "before the project folder is read the key says so instead of doing nothing"
    );
}

#[tokio::test]
async fn settings_plan_roadmap_now_starts_the_job() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-roadmap-")
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
    let project = ProjectId::new("roadmap-later").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Roadmap later".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "A notes app made before roadmap planning.".into(),
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
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));

    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let start = 1_793_000_000_000;
    settle(&mut screen, &mut controller, start).await;
    assert!(
        screen
            .jobs
            .as_ref()
            .unwrap()
            .iter()
            .all(|job| job.kind != ResearchKind::RoadmapPlanning),
        "the toggle was off, so nothing planned on its own"
    );
    screen.open(Tab::Settings);
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(
        text(&drawn.rows).contains("Roadmap   Not planned   p Plan roadmap now"),
        "{}",
        text(&drawn.rows)
    );
    assert_eq!(screen.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    let mut earlier = screen.general.take().expect("the General form opens");
    earlier.research.push(ResearchKind::MarketResearch);

    let ProjectReply::Change(action) = screen.key(key(KeyCode::Char('p'))) else {
        panic!("p asks the engine to plan");
    };
    assert_eq!(
        *action,
        SetupAction::PlanRoadmap {
            project_id: project.clone(),
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
    settle(&mut screen, &mut controller, start + 1_000).await;
    let planned: Vec<_> = screen
        .jobs
        .as_ref()
        .unwrap()
        .iter()
        .filter(|job| job.kind == ResearchKind::RoadmapPlanning)
        .map(|job| job.status)
        .collect();
    assert_eq!(planned.len(), 1, "one planning job is queued");
    assert!(
        matches!(
            planned[0],
            ResearchStatus::Queued | ResearchStatus::NeedsPool | ResearchStatus::Running
        ),
        "{planned:?}"
    );
    let drawn = project_screen(&mut screen, 80, 24, &look);
    assert!(
        text(&drawn.rows).contains("Roadmap   Planning"),
        "{}",
        text(&drawn.rows)
    );

    assert_eq!(screen.key(key(KeyCode::Char('p'))), ProjectReply::Handled);
    assert_eq!(
        screen.problem.as_deref(),
        Some("A roadmap is already being planned for this project. Wait for it to finish.")
    );
    let again = serve(
        &mut controller,
        SetupQuery::Change {
            action: SetupAction::PlanRoadmap {
                project_id: project.clone(),
            },
            generation: 3,
        },
    )
    .await;
    assert_eq!(
        again,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "planning_busy".into()
        })),
        "the engine refuses a second job while one is queued"
    );
    let stored = controller
        .project_setup(project.clone())
        .await
        .unwrap()
        .expect("the setup is saved");
    assert_eq!(
        (stored.research.clone(), stored.revision),
        (vec![ResearchKind::RoadmapPlanning], 2),
        "the setup records planning at a new revision"
    );
    settle(&mut screen, &mut controller, start + 2_000).await;
    let current = screen.setup.clone().expect("the setup is read again");
    assert_eq!(current.revision, 2, "the screen reads the new revision");
    assert!(
        screen.work_kinds().is_empty(),
        "planning is not listed as research on the Overview"
    );
    let Some(SetupAction::Update {
        expected_revision,
        setup: edited,
        ..
    }) = earlier.action(&current)
    else {
        panic!("the earlier form has an edit");
    };
    assert_eq!(
        (edited.research.clone(), expected_revision),
        (
            vec![ResearchKind::MarketResearch, ResearchKind::RoadmapPlanning],
            1
        ),
        "a form opened before Plan roadmap now keeps the planning choice"
    );
    let stale = serve(
        &mut controller,
        SetupQuery::Change {
            action: SetupAction::Update {
                project_id: project.clone(),
                kept: None,
                expected_revision,
                name: None,
                setup: edited,
            },
            generation: 4,
        },
    )
    .await;
    assert_eq!(
        stale,
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: "stale_revision".into()
        })),
        "an edit drafted before the plan cannot cancel it"
    );
}

/// A document longer than one chunk is opened, another file is opened while its second
/// chunk is in flight, then the first is opened again: the late chunk never starts the new
/// preview, which reads from offset 0 and shows the whole document in order.
#[tokio::test]
async fn reopening_a_file_while_a_chunk_is_in_flight_reads_it_from_the_start() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-chunk-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sources = tempfile::tempdir().unwrap();
    let long: String = (1..=2000).map(|line| format!("line {line:05}\n")).collect();
    assert!(long.len() > 2 * crate::project_setup::CONTEXT_CHUNK_BYTES);
    std::fs::write(sources.path().join("long.md"), &long).unwrap();
    std::fs::write(sources.path().join("short.md"), "# Short\n").unwrap();
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
    let project = ProjectId::new("chunks").unwrap();
    let document = |name: &str| sources.path().join(name).to_str().unwrap().to_owned();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Chunks".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Long documents.".into(),
                tech_stack: vec!["rust".into()],
                platforms: vec![Platform::Cli],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![document("long.md"), document("short.md")],
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
    let now = 1_793_000_000_000;
    settle(&mut screen, &mut controller, now).await;
    let long_path = "context/documents/long.md";
    let item = |offset: u64| SetupQuery::Item {
        project: project.clone(),
        path: long_path.into(),
        offset,
    };

    screen.read(long_path);
    let first = screen.next_query(now).unwrap();
    assert_eq!(first, item(0));
    send(&mut screen, &mut controller, first).await;
    let second = screen.next_query(now).unwrap();
    let SetupQuery::Item { offset, .. } = &second else {
        panic!("the preview reads its next chunk: {second:?}");
    };
    assert!(*offset > 0, "{second:?}");
    let in_flight = serve(&mut controller, second.clone()).await;
    screen.read("context/documents/short.md");
    screen.read(long_path);
    screen.apply(&second, in_flight);

    let restart = screen.next_query(now);
    assert_eq!(
        restart,
        Some(item(0)),
        "the reopened preview reads from the start"
    );
    send(&mut screen, &mut controller, restart.unwrap()).await;
    for _ in 0..8 {
        let Some(query) = screen.next_query(now) else {
            break;
        };
        send(&mut screen, &mut controller, query).await;
    }
    let preview = screen.preview.as_ref().unwrap();
    assert_eq!(preview.path, long_path);
    assert_eq!(preview.next, None);
    assert_eq!(preview.text, long, "the whole document, in order");
}

/// A stack entry may hold a comma, which the engine allows: saving the General form after
/// changing only the name keeps every stack entry as it was.
#[test]
fn the_general_form_keeps_stack_entries_holding_commas_when_the_stack_is_untouched() {
    let project = ProjectId::new("commas").unwrap();
    let mut setup = stored(&project);
    setup.tech_stack = vec!["C, C++ interop".into(), "rust".into()];
    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let first = screen.next_query(10).unwrap();
    screen.apply(&first, Ok(SetupView::Setup(Some(Box::new(setup.clone())))));
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    let control_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(screen.key(control_u), ProjectReply::Handled);
    assert_eq!(screen.paste("Notes Pro"), ProjectReply::Handled);

    let reply = screen.key(key(KeyCode::Enter));

    let ProjectReply::Change(action) = reply else {
        panic!("the name change is sent: {reply:?}");
    };
    match *action {
        SetupAction::Update {
            name, setup: sent, ..
        } => {
            assert_eq!(name.as_deref(), Some("Notes Pro"));
            assert_eq!(sent.tech_stack, setup.tech_stack);
        }
        other => panic!("the form sends an update: {other:?}"),
    }
}

/// Appending to a stack whose saved entry holds a comma keeps that entry whole: only the
/// text the owner typed splits on commas.
#[test]
fn editing_the_stack_keeps_saved_entries_holding_commas_whole() {
    let project = ProjectId::new("commas").unwrap();
    let mut setup = stored(&project);
    setup.tech_stack = vec!["C, C++ interop".into(), "rust".into()];
    let mut screen = ProjectScreen::new(project.clone(), "loading".into(), None);
    let first = screen.next_query(10).unwrap();
    screen.apply(&first, Ok(SetupView::Setup(Some(Box::new(setup.clone())))));
    screen.open(Tab::Settings);
    assert_eq!(screen.key(key(KeyCode::Char('g'))), ProjectReply::Handled);
    screen.key(key(KeyCode::Tab));
    screen.key(key(KeyCode::Tab));
    assert_eq!(screen.general.as_ref().unwrap().cursor, 2);
    assert_eq!(screen.paste(", go, kotlin"), ProjectReply::Handled);

    let reply = screen.key(key(KeyCode::Enter));

    let ProjectReply::Change(action) = reply else {
        panic!("the stack change is sent: {reply:?}");
    };
    match *action {
        SetupAction::Update { setup: sent, .. } => {
            assert_eq!(sent.tech_stack, ["C, C++ interop", "rust", "go", "kotlin"]);
        }
        other => panic!("the form sends an update: {other:?}"),
    }
}

#[test]
fn a_live_switch_that_opened_no_session_shows_on_and_names_why() {
    let project = ProjectId::new("unopened").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Unopened".into(), None);
    let query = SetupQuery::TeamChange {
        action: crate::team::TeamAction::LiveMaster {
            project_id: project.clone(),
            on: true,
            version: 0,
        },
        generation: 1,
    };
    screen.apply(
        &query,
        Ok(SetupView::LiveWithoutSession(
            "No account pool is assigned to this project as its master".into(),
        )),
    );
    assert_eq!(
        screen.problem.as_deref(),
        Some(
            "No master session opened, so this conversation won't be in Sessions. No account pool is assigned to this project as its master"
        )
    );
    screen.apply(
        &query,
        Ok(SetupView::LiveWithoutSession(
            "master_open_provenance".into(),
        )),
    );
    let named = screen.problem.clone().unwrap();
    assert!(
        named.starts_with("No master session opened, so this conversation won't be in Sessions. ")
            && named.contains("master_open_provenance"),
        "{named}"
    );
}

/// A short window shows the help lines of the setting the cursor is on along with its row, so
/// the owner reads what the setting does while changing it (FB-TUI F1).
#[test]
fn the_access_box_shows_the_help_of_the_row_the_cursor_is_on_in_a_short_window() {
    use super::project_access::{IDE_GIT_HOOKS_LINE, agent_files_help, network_help};
    use crate::sandbox::policy::{AgentNetwork, IdeAgentFiles, ToolchainView};
    use crate::sandbox::toolchains::Toolchain;
    let opening = |help: &str| help.split(' ').take(5).collect::<Vec<_>>().join(" ");
    let project = ProjectId::new("access-help").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    let read = next_access_read(&mut screen, 20);
    let mut view = access_view(AgentAccess::Sandboxed, 1);
    view.toolchains = [(Toolchain::Rust, "Rust"), (Toolchain::Python, "Python")]
        .into_iter()
        .map(|(toolchain, label)| ToolchainView {
            toolchain,
            label: label.into(),
            installed: true,
            used: false,
            allowed: false,
        })
        .collect();
    screen.apply(&read, Ok(SetupView::AgentPolicy(view)));
    screen.key(key(KeyCode::Char('c')));
    let look = Look::plain();
    for _ in 0..3 {
        screen.key(key(KeyCode::Down));
    }
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("> Network")
            && shown.contains(&opening(network_help(AgentNetwork::default())))
            && shown.contains("[> Save ]"),
        "{shown}"
    );
    screen.key(key(KeyCode::Down));
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("> [ ] IDE git runs the project's hooks")
            && shown.contains(&opening(IDE_GIT_HOOKS_LINE))
            && shown.contains("[> Save ]"),
        "{shown}"
    );
    screen.key(key(KeyCode::Down));
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("> IDE changes in agent folders  < Allow all changes >")
            && shown.contains(&opening(agent_files_help(IdeAgentFiles::AllowAll)))
            && shown.contains("[> Save ]"),
        "{shown}"
    );
}

/// The Agent access box keeps its save line, or the Full access warning with its y and n, on
/// screen when the window is shorter than the box, and the settings above follow the cursor
/// down to the IDE hooks switch with the lines left out counted (FB-TUI row 3). The Full
/// access warning is checked on macOS only: Linux offers Sandboxed alone in this version
/// (`LINUX_UNAVAILABLE`), so there the level stays and Enter saves.
#[test]
fn the_access_box_keeps_its_save_line_and_the_cursor_row_in_a_short_window() {
    use crate::sandbox::policy::ToolchainView;
    use crate::sandbox::toolchains::Toolchain;
    let project = ProjectId::new("access-short").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Settings);
    let read = next_access_read(&mut screen, 20);
    let mut view = access_view(AgentAccess::Sandboxed, 1);
    view.toolchains = [(Toolchain::Rust, "Rust"), (Toolchain::Python, "Python")]
        .into_iter()
        .map(|(toolchain, label)| ToolchainView {
            toolchain,
            label: label.into(),
            installed: true,
            used: false,
            allowed: false,
        })
        .collect();
    screen.apply(&read, Ok(SetupView::AgentPolicy(view)));
    screen.key(key(KeyCode::Char('c')));
    let look = Look::plain();
    for (width, height) in [(120, 40), (80, 24)] {
        let drawn = project_screen(&mut screen, width, height, &look);
        assert_eq!(drawn.rows.len(), usize::from(height) - 4);
        let shown = text(&drawn.rows);
        assert!(
            shown.contains("> Access      < Sandboxed >")
                && shown.contains("[> Save ]")
                && shown.contains("more lines below"),
            "{width}x{height}:\n{shown}"
        );
    }
    let shown = text(&project_screen(&mut screen, 120, 40, &look).rows);
    assert!(
        shown.contains("Toolchains agents may use") && shown.contains("[ ] Rust"),
        "{shown}"
    );
    for _ in 0..4 {
        screen.key(key(KeyCode::Down));
    }
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    assert!(
        shown.contains("> [ ] IDE git runs the project's hooks")
            && shown.contains("more lines above")
            && shown.contains("[> Save ]"),
        "{shown}"
    );
    for _ in 0..4 {
        screen.key(key(KeyCode::Up));
    }
    screen.key(key(KeyCode::Right));
    screen.key(key(KeyCode::Right));
    screen.key(key(KeyCode::Enter));
    let shown = text(&project_screen(&mut screen, 80, 24, &look).rows);
    if cfg!(target_os = "macos") {
        assert!(
            shown.contains("Full access: Agents run with your user's permissions")
                && shown.contains("[> y Allow Full access ]"),
            "{shown}"
        );
    } else if cfg!(target_os = "linux") {
        assert!(
            shown.contains("> Access      < Sandboxed >") && shown.contains("Saving..."),
            "{shown}"
        );
    }
}

/// The Settings key m asks before a project moves into the projects home: the ask says the
/// team must be stopped and its work follows, m sends a Move, c a Copy, and n or Esc keeps the
/// project where it is. A refusal shows as a plain line and an applied move reads the setup
/// again.
#[test]
fn move_into_the_projects_home_asks_first_and_sends_move_or_copy() {
    use crate::projects_home::AddMode;
    let project = ProjectId::new("kept").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Kept".into(), None);
    screen.open(Tab::Settings);
    let shown = text(&project_screen(&mut screen, 120, 30, &Look::plain()).rows);
    assert!(shown.contains("m Move into CrazyAgent folder"), "{shown}");
    assert_eq!(screen.key(key(KeyCode::Char('m'))), ProjectReply::Handled);
    assert!(screen.confirm_move);
    let drawn = project_screen(&mut screen, 80, 24, &Look::plain());
    let shown = text(&drawn.rows);
    assert!(
        shown.contains("Move Kept into your CrazyAgent folder?"),
        "{shown}"
    );
    assert!(shown.contains("Stop the team first"), "{shown}");
    assert!(shown.contains("[> Keep it here ]"), "{shown}");
    assert_eq!(
        drawn.help,
        [
            ("m", "Move"),
            ("c", "Copy"),
            ("n", "Keep it here"),
            ("Esc", "Keep it here")
        ]
    );
    assert_eq!(screen.key(key(KeyCode::Char('q'))), ProjectReply::Handled);
    assert!(screen.confirm_move);
    assert_eq!(screen.key(key(KeyCode::Esc)), ProjectReply::Handled);
    assert!(!screen.confirm_move);
    screen.key(key(KeyCode::Char('m')));
    assert_eq!(screen.key(key(KeyCode::Char('n'))), ProjectReply::Handled);
    assert!(!screen.confirm_move);
    screen.key(key(KeyCode::Char('m')));
    assert_eq!(
        screen.key(key(KeyCode::Char('m'))),
        ProjectReply::MoveHome(AddMode::Move)
    );
    assert!(!screen.confirm_move);
    screen.key(key(KeyCode::Char('m')));
    assert_eq!(
        screen.key(key(KeyCode::Char('c'))),
        ProjectReply::MoveHome(AddMode::Copy)
    );
    assert!(!screen.confirm_move);

    let query = SetupQuery::MoveHome {
        project,
        mode: AddMode::Move,
        generation: 4,
    };
    let refused = |code: &str| {
        Ok(SetupView::Changed(OwnerResult::Rejected {
            code: code.into(),
        }))
    };
    screen.apply(&query, refused("move_project_busy"));
    assert!(
        screen
            .problem
            .as_deref()
            .is_some_and(|problem| problem.starts_with("Stop the team first")),
        "{:?}",
        screen.problem
    );
    screen.apply(&query, refused("move_project_already_home"));
    assert_eq!(
        screen.problem.as_deref(),
        Some("This project is already in your projects folder.")
    );
    screen.apply(&query, refused("home_move_worktrees"));
    assert!(
        screen
            .problem
            .as_deref()
            .is_some_and(|problem| !problem.contains("home_move_worktrees")),
        "{:?}",
        screen.problem
    );
    screen.problem = None;
    screen.setup_due = false;
    screen.apply(&query, Ok(SetupView::Changed(OwnerResult::Applied)));
    assert_eq!(screen.problem, None);
    assert!(screen.setup_due);
}

#[test]
fn a_file_linked_from_the_repo_says_so_opens_from_the_project_folder_and_is_not_deleted() {
    use super::project::OpenRequest;
    use crate::project_setup::{ContextItem, ProjectContext};
    let project = ProjectId::new("notes-cli").unwrap();
    let mut screen = loaded(&project);
    screen.open(Tab::Context);
    screen.context = Some(ProjectContext {
        project_id: project.clone(),
        folder: "/private/tmp/ca-notes-cli/.crazyagent".into(),
        items: vec![ContextItem {
            path: "repo/docs/plan-2026.md".into(),
            kind: ContextKind::Plan,
            bytes: 15,
            linked: true,
        }],
    });
    let look = Look::plain();
    let drawn = project_screen(&mut screen, 80, 24, &look);
    let shown = text(&drawn.rows);
    assert!(shown.contains("plan-2026.md"), "{shown}");
    assert!(shown.contains("Linked from your repo"), "{shown}");
    assert!(!shown.contains("Planned after research"), "{shown}");

    let plan = OpenRequest::Context {
        folder: "/private/tmp/ca-notes-cli".into(),
        path: "docs/plan-2026.md".into(),
    };
    assert_eq!(
        screen.key(key(KeyCode::Char('o'))),
        ProjectReply::Open(plan.clone()),
        "a linked file opens from the project folder, not the state folder"
    );
    assert_eq!(
        screen.key(key(KeyCode::Char('v'))),
        ProjectReply::Edit(plan)
    );
    assert_eq!(screen.key(key(KeyCode::Enter)), ProjectReply::Handled);
    assert!(screen.reading, "the linked file opens in the reader");
    screen.key(key(KeyCode::Esc));
    assert!(!screen.reading);
    assert_eq!(screen.key(key(KeyCode::Char('d'))), ProjectReply::Handled);
    assert!(screen.confirm_delete.is_none(), "no delete is asked for");
    assert_eq!(
        screen.problem.as_deref(),
        Some("This file is linked from your repo. It can't be deleted here.")
    );
}

mod overview_read {
    use super::*;
    use crate::project_overview::{Apply, Competitors, Overview, Tab as OverviewTab, TabState};
    use crate::roadmap_status::Refresh;

    const STEP: i64 = super::super::project::REFRESH_MS;

    fn overview() -> Overview {
        Overview {
            readme: None,
            tabs: OverviewTab::ALL
                .into_iter()
                .map(|tab| TabState {
                    tab,
                    text: None,
                    version: String::new(),
                    edited: false,
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
            refresh: Refresh::AfterLanding,
            apply: Apply::AlwaysAsk,
            needs_home: false,
        }
    }

    /// Answers every other read as failed until the screen asks for the Overview tabs.
    fn overview_asked(screen: &mut ProjectScreen, now: i64) -> Option<SetupQuery> {
        for _ in 0..16 {
            let query = screen.next_query(now)?;
            if matches!(query, SetupQuery::Overview { .. }) {
                return Some(query);
            }
            screen.apply(&query, Err("engine_unavailable".into()));
        }
        panic!("reads never settled");
    }

    fn read(screen: &mut ProjectScreen, now: i64, overview: Overview) {
        let query = overview_asked(screen, now).expect("the tabs are read");
        screen.apply(&query, Ok(SetupView::Overview(Box::new(overview))));
    }

    #[test]
    fn the_overview_tab_reads_the_tabs_once_and_again_only_while_something_is_pending() {
        let project = ProjectId::new("notes-cli").unwrap();
        let mut screen = loaded(&project);
        let asked = overview_asked(&mut screen, 100).unwrap();
        assert_eq!(
            asked,
            SetupQuery::Overview {
                project: project.clone()
            }
        );
        screen.apply(&asked, Ok(SetupView::Overview(Box::new(overview()))));
        assert!(screen.overview_tabs.is_some());
        assert_eq!(
            overview_asked(&mut screen, 100 + 10 * STEP),
            None,
            "nothing is pending"
        );

        let mut running = overview();
        running.running = true;
        let asked = SetupQuery::RefreshOverview {
            project: project.clone(),
        };
        screen.apply(&asked, Ok(SetupView::OverviewAsked));
        read(&mut screen, 200_000, running);
        assert_eq!(
            overview_asked(&mut screen, 200_000 + STEP - 1),
            None,
            "not before the wait"
        );
        assert!(
            overview_asked(&mut screen, 200_000 + STEP).is_some(),
            "a run is read again"
        );

        let mut waiting = overview();
        waiting.stale = true;
        screen.apply(
            &SetupQuery::Overview {
                project: project.clone(),
            },
            Ok(SetupView::Overview(Box::new(waiting.clone()))),
        );
        let later = 200_000 + 3 * STEP;
        assert!(
            overview_asked(&mut screen, later).is_some(),
            "a landing waits for a run"
        );

        for (round, (off, needs_home)) in [(true, false), (false, true)].into_iter().enumerate() {
            let checked = later + (round as i64 + 1) * 10 * STEP;
            let mut held = waiting.clone();
            held.needs_home = needs_home;
            if off {
                held.refresh = Refresh::Off;
            }
            screen.apply(
                &SetupQuery::Overview {
                    project: project.clone(),
                },
                Ok(SetupView::Overview(Box::new(held))),
            );
            assert_eq!(
                overview_asked(&mut screen, checked),
                None,
                "no run can start"
            );
        }
    }

    #[test]
    fn another_tab_does_not_read_the_overview_tabs() {
        let project = ProjectId::new("notes-cli").unwrap();
        let mut screen = loaded(&project);
        screen.open(Tab::Context);
        assert_eq!(overview_asked(&mut screen, 100), None);
        assert!(screen.overview_tabs.is_none());
    }

    #[test]
    fn a_failed_read_keeps_the_last_tabs_and_is_tried_again_after_the_wait() {
        let project = ProjectId::new("notes-cli").unwrap();
        let mut screen = loaded(&project);
        let asked = overview_asked(&mut screen, 100).unwrap();
        screen.apply(&asked, Err("overview_unavailable".into()));
        assert!(screen.overview_tabs.is_none());
        assert_eq!(
            screen.problem.as_deref(),
            Some("The Overview tabs can't be read right now. Try again in a moment.")
        );
        assert_eq!(overview_asked(&mut screen, 100 + STEP - 1), None);
        let mut running = overview();
        running.running = true;
        read(&mut screen, 100 + STEP, running);
        let again = overview_asked(&mut screen, 100 + 2 * STEP).unwrap();
        screen.apply(&again, Err("overview_unavailable".into()));
        assert!(
            screen
                .overview_tabs
                .as_ref()
                .is_some_and(|kept| kept.running)
        );
    }

    #[test]
    fn shift_r_asks_for_a_run_or_says_why_none_can_start() {
        let project = ProjectId::new("notes-cli").unwrap();
        let mut screen = loaded(&project);
        let refresh = shifted(KeyCode::Char('R'));
        assert_ne!(
            screen.key(refresh),
            ProjectReply::RefreshOverview,
            "not before the read"
        );
        read(&mut screen, 100, overview());
        assert_eq!(screen.key(refresh), ProjectReply::RefreshOverview);

        let asked = SetupQuery::RefreshOverview {
            project: project.clone(),
        };
        screen.apply(&asked, Ok(SetupView::OverviewAsked));
        assert!(
            overview_asked(&mut screen, 101).is_some(),
            "the ask is followed by a read"
        );

        type Change = fn(&mut Overview);
        let cases: [(Change, &str); 3] = [
            (
                |o| o.running = true,
                "A run is writing the tabs now. They fill in when it ends.",
            ),
            (
                |o| o.refresh = Refresh::Off,
                "Overview tabs are off for this project. Turn them on in Project settings.",
            ),
            (
                |o| o.needs_home = true,
                "Overview tabs need this project in the CrazyAgent folder. Move it there in the desktop app, in Project settings.",
            ),
        ];
        for (change, line) in cases {
            let mut held = overview();
            change(&mut held);
            screen.overview_tabs = Some(held);
            screen.problem = None;
            assert_eq!(screen.key(refresh), ProjectReply::Handled, "{line}");
            assert_eq!(screen.problem.as_deref(), Some(line));
        }
    }

    #[test]
    fn a_refused_refresh_says_why_and_reads_the_tabs_again() {
        let project = ProjectId::new("notes-cli").unwrap();
        let mut screen = loaded(&project);
        read(&mut screen, 100, overview());
        let asked = SetupQuery::RefreshOverview { project };
        screen.apply(&asked, Err("overview_off".into()));
        assert_eq!(
            screen.problem.as_deref(),
            Some("Overview tabs are off for this project. Turn them on in Project settings.")
        );
        assert!(overview_asked(&mut screen, 101).is_some());
    }
}
