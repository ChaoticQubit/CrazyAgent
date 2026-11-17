use super::*;

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
    let runs_again = crate::project_setup::retryable(ResearchStatus::Cancelled);
    assert_eq!(drawn.help.contains(&("r", "Retry")), runs_again);
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
    if !runs_again {
        let asked = SetupQuery::Change {
            action: *action,
            generation: 3,
        };
        let refused = serve(&mut controller, asked.clone()).await;
        assert_eq!(
            refused,
            Ok(SetupView::Changed(OwnerResult::Rejected {
                code: "research_unavailable_here".into()
            }))
        );
        screen.apply(&asked, refused);
        assert_eq!(
            screen.problem.as_deref(),
            Some("Research does not run on this computer in this version, so it cannot run again.")
        );
        return;
    }
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
