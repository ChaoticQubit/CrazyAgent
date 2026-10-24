use super::*;
use crate::engine::bootstrap::EngineService;
use crate::projects_home::FoundState;
use std::os::unix::fs::PermissionsExt;

fn private_root(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/private/tmp")
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
    let home = root.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(home.join("notes")).unwrap();
    std::fs::write(home.join("notes").join("README.md"), "notes\n").unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("plan.txt"), "12345").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(root.path().join("e.sock"), Duration::from_secs(2));
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
        .intake(IntakeJob::Query(IntakeQuery::FolderSize {
            path: source.clone(),
        }))
        .await;
    assert_eq!(event.result, Ok(IntakeView::FolderSize(5)));

    let event = native
        .intake(IntakeJob::Query(IntakeQuery::AddExisting {
            source,
            mode: AddMode::Copy,
            generation: 2,
        }))
        .await;
    assert!(
        matches!(event.result, Ok(IntakeView::Changed(OwnerResult::Applied))),
        "{:?}",
        event.result
    );
    assert!(
        outside.join("plan.txt").exists(),
        "a copy leaves the source"
    );
    let event = native.intake(IntakeJob::Query(IntakeQuery::Found)).await;
    let Ok(IntakeView::Found(found)) = event.result else {
        panic!("found folders required: {:?}", event.result);
    };
    assert!(
        found
            .iter()
            .any(|project| project.name == "outside" && project.state == FoundState::Found),
        "{found:?}"
    );

    service.shutdown().await.unwrap();
}
