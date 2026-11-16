use super::*;
use crate::engine::bootstrap::EngineService;
use crate::projects_home::FoundState;
use std::os::unix::fs::PermissionsExt;

fn private_root(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn a_token_job_answers_as_a_status_read_and_never_shows_the_token() {
    let job =
        IntakeJob::GitHubToken(AccountSecret::new(b"ghp_intake_sample_value".to_vec()).unwrap());
    assert_eq!(job.query(), IntakeQuery::GitHubStatus);
    let event = IntakeEvent::unavailable(&job);
    assert_eq!(event.query, IntakeQuery::GitHubStatus);
    assert_eq!(event.result, Err("trusted_connection_unavailable".into()));
    let shown = format!("{event:?}");
    assert!(!shown.contains("ghp_"), "{shown}");
}

#[test]
fn a_started_onboarding_is_named_like_the_engine_names_it() {
    assert_eq!(
        started("/Users/owner/Projects/My Notes_v2"),
        Ok(ProjectId::new("my-notes-v2").unwrap())
    );
    assert_eq!(
        started("/Users/owner/Projects/My Notes_v2"),
        crate::onboarding::project_id_for(std::path::Path::new(
            "/Users/owner/Projects/My Notes_v2"
        ))
        .map_err(|error| error.0)
    );
    assert_eq!(started("/"), Err("onboarding_path_invalid".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_sets_the_projects_home_and_reads_its_folders_from_a_real_engine() {
    let root = private_root("ca-tintake-");
    let work = private_root("ca-tintake-work-");
    let home = std::fs::canonicalize(work.path()).unwrap().join("Projects");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(home.join("notes")).unwrap();
    std::fs::write(home.join("notes").join("README.md"), "notes\n").unwrap();
    let outside = home.parent().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("plan.txt"), "12345").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));

    let event = native.intake(IntakeJob::Query(IntakeQuery::Home)).await;
    assert_eq!(event.query, IntakeQuery::Home);
    let Ok(IntakeView::Home(first)) = event.result else {
        panic!("projects home read required: {:?}", event.result);
    };
    assert_eq!(first.path, None);

    let path = home.to_str().unwrap().to_owned();
    let event = native
        .intake(IntakeJob::Query(IntakeQuery::SetHome {
            path: path.clone(),
            generation: 1,
        }))
        .await;
    assert!(
        matches!(event.result, Ok(IntakeView::Changed(OwnerResult::Applied))),
        "{:?}",
        event.result
    );
    let event = native.intake(IntakeJob::Query(IntakeQuery::Home)).await;
    let Ok(IntakeView::Home(set)) = event.result else {
        panic!("projects home read required: {:?}", event.result);
    };
    assert_eq!(set.path.as_deref(), Some(path.as_str()));

    let event = native.intake(IntakeJob::Query(IntakeQuery::Found)).await;
    let Ok(IntakeView::Found(found)) = event.result else {
        panic!("found folders required: {:?}", event.result);
    };
    let notes = found
        .iter()
        .find(|project| project.name == "notes")
        .expect("the notes folder is found");
    assert_eq!(notes.state, FoundState::Found);
    assert_eq!(notes.project_id, None);

    let source = outside.to_str().unwrap().to_owned();
    let event = native
        .intake(IntakeJob::Query(IntakeQuery::FolderSize { path: source }))
        .await;
    assert_eq!(
        event.result,
        Ok(IntakeView::FolderSize(crate::projects_home::Measured {
            bytes: 5,
            placeholders: 0
        }))
    );

    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_starts_an_intake_job_reads_it_to_done_and_dismisses_it() {
    let root = private_root("ca-tjob-");
    let work = private_root("ca-tjob-work-");
    let home = std::fs::canonicalize(work.path()).unwrap().join("Projects");
    std::fs::create_dir(&home).unwrap();
    let outside = home.parent().unwrap().join("site");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("index.html"), "<p>hi</p>").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let set = IntakeQuery::SetHome {
        path: home.to_str().unwrap().to_owned(),
        generation: 1,
    };
    let event = native.intake(IntakeJob::Query(set)).await;
    assert_eq!(event.result, Ok(IntakeView::Changed(OwnerResult::Applied)));

    let source = outside.to_str().unwrap().to_owned();
    let id = start_id().unwrap();
    let start = IntakeQuery::JobStart {
        id: id.clone(),
        subject: crate::projects_home::job::JobSubject::Add {
            path: source.clone(),
        },
        mode: AddMode::Copy,
        overrides: Vec::new(),
    };
    let event = native.intake(IntakeJob::Query(start.clone())).await;
    assert_eq!(event.query, start);
    let Ok(IntakeView::Job(Some(started))) = event.result else {
        panic!("a started job required: {:?}", event.result);
    };
    assert_eq!(started.source, source);
    assert_eq!(started.id, id, "the job carries the start's id");
    let mut done = None;
    for _ in 0..200 {
        let event = native
            .intake(IntakeJob::Query(IntakeQuery::JobStatus))
            .await;
        let Ok(IntakeView::Job(Some(job))) = event.result else {
            panic!("the job's status required: {:?}", event.result);
        };
        if job.state == crate::projects_home::job::JobState::Done {
            done = Some(job);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let done = done.expect("the copy job ends done");
    assert_eq!(done.id, started.id);
    assert!(
        home.join("site").join("index.html").exists(),
        "the copy landed"
    );
    assert!(
        outside.join("index.html").exists(),
        "a copy leaves the source"
    );
    let dismiss = IntakeQuery::JobDismiss { id: done.id };
    let event = native.intake(IntakeJob::Query(dismiss)).await;
    assert_eq!(event.result, Ok(IntakeView::JobDismissed(true)));
    let event = native
        .intake(IntakeJob::Query(IntakeQuery::JobStatus))
        .await;
    assert_eq!(event.result, Ok(IntakeView::Job(None)));

    service.shutdown().await.unwrap();
}

async fn owner_event(connection: &mut Connection) -> OwnerEvent {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(event) = connection.poll_owner().unwrap() {
            return event;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "owner event timed out"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn rule_set() -> IntakeQuery {
    IntakeQuery::GitHubRuleSet {
        project: ProjectId::new("alpha").unwrap(),
        repo: "octo-cat/tool".into(),
        rule: GitHubRule::WikiOff,
        on: true,
    }
}

/// A GitHub rule switch runs on a lane of its own (rv-ds241 gh-s6c N1): the worker answers
/// it at once as changing and takes the next request, and the switch's own result comes
/// after as an event of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rule_switch_is_answered_at_once_and_its_result_comes_after() {
    let root = private_root("ca-tlane-");
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut connection = Connection::connect_paired(
        Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    while !matches!(
        owner_event(&mut connection).await,
        OwnerEvent::Access { ready: true, .. }
    ) {}
    let set = rule_set();
    connection.intake(IntakeJob::Query(set.clone())).unwrap();
    let OwnerEvent::Intake(first) = owner_event(&mut connection).await else {
        panic!("the switch is answered first");
    };
    assert_eq!(
        *first,
        IntakeEvent {
            query: set.clone(),
            result: Ok(IntakeView::RuleChanging),
        }
    );
    assert!(!connection.busy(), "the worker takes the next request");
    connection
        .intake(IntakeJob::Query(IntakeQuery::Home))
        .unwrap();
    let (mut home, mut result) = (None, None);
    while home.is_none() || result.is_none() {
        let OwnerEvent::Intake(event) = owner_event(&mut connection).await else {
            continue;
        };
        if event.query == IntakeQuery::Home {
            home = Some(event.result);
        } else if event.query == set {
            result = Some(event.result);
        }
    }
    assert!(matches!(home, Some(Ok(IntakeView::Home(_)))), "{home:?}");
    let result = result.unwrap();
    assert!(result.is_err(), "alpha is no project: {result:?}");
    drop(connection);
    service.shutdown().await.unwrap();
}

/// One rule switch at a time: while one runs, the next is refused at once with
/// `github_rule_busy` and sends nothing later; once it ends, the next one starts and its
/// result comes as an unfinished event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_rule_switch_while_one_runs_is_refused_at_once() {
    let root = private_root("ca-tlane2-");
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let (replies, receiver) = std::sync::mpsc::sync_channel(1);
    let (release, held) = std::sync::mpsc::channel::<()>();
    native.rule_lane = Some(std::thread::spawn(move || {
        let _ = held.recv();
    }));
    let set = rule_set();
    assert_eq!(
        native.rule_lane(IntakeJob::Query(set.clone()), &replies),
        IntakeEvent {
            query: set.clone(),
            result: Err("github_rule_busy".into()),
        }
    );
    assert!(
        receiver.try_recv().is_err(),
        "a refused switch sends nothing"
    );
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !native.rule_lane.as_ref().unwrap().is_finished() {
        assert!(std::time::Instant::now() < deadline, "the held lane ends");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        native.rule_lane(IntakeJob::Query(set.clone()), &replies),
        IntakeEvent {
            query: set.clone(),
            result: Ok(IntakeView::RuleChanging),
        }
    );
    let reply =
        tokio::task::block_in_place(|| receiver.recv_timeout(std::time::Duration::from_secs(20)))
            .unwrap();
    assert!(!reply.finished, "the result is not the worker's answer");
    let OwnerEvent::Intake(event) = reply.event else {
        panic!("the switch's result is an intake event");
    };
    assert_eq!(event.query, set);
    assert!(event.result.is_err(), "{:?}", event.result);
    service.shutdown().await.unwrap();
}
