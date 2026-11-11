use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerResponse;
use crate::project_setup::{Platform, ProjectSetup};
use crate::protocol::ProjectSpec;
use crate::questions::{Ask, Asker, LISTED_BYTES_MAX, QUESTION_BYTES_MAX};
use std::os::unix::fs::PermissionsExt;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-tquestions-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn long_ask(index: usize) -> Ask {
    let mut question = format!("Question {index:02}: which layout should page {index:02} use? ");
    question.push_str(&"a".repeat(QUESTION_BYTES_MAX - question.len()));
    Ask {
        question,
        kind: crate::questions::QuestionKind::Single,
        options: vec!["Grid".into(), "List".into()],
        recommended: Some("Grid".into()),
        context: Some("c".repeat(crate::questions::CONTEXT_BYTES_MAX)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_worker_lists_every_pending_question_and_maps_each_request_through_the_engine() {
    let root = private_root();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let controller = native.controller.as_mut().unwrap();
    let _home = crate::tui::test_home::projects_home(controller, 1).await;
    let project = ProjectId::new("layouts").unwrap();
    let created = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Layouts".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Page layouts.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            2,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));

    let db = crate::questions::open(root.path()).unwrap();
    let pending_max = 2 * crate::questions::PENDING_PER_ASKER_MAX as usize;
    let mut ids = Vec::new();
    for index in 0..pending_max {
        let asker = Asker::Master(1 + (index % 2) as i64);
        ids.push(crate::questions::ask(&db, &project, &asker, &long_ask(index)).unwrap());
    }
    let settled = ids.remove(0);
    let first = native
        .controller
        .as_ref()
        .unwrap()
        .questions_list(project.clone(), None)
        .await
        .unwrap();
    assert!(
        first.next.is_some() && first.items.len() < pending_max,
        "the test needs pending questions past one page of {LISTED_BYTES_MAX} bytes: {}",
        first.items.len()
    );

    let dismiss = QuestionsQuery::Dismiss {
        project: project.clone(),
        id: settled,
        version: 0,
    };
    let event = native.questions(dismiss.clone()).await;
    assert_eq!(event.query, dismiss);
    assert_eq!(
        event.result,
        Ok(QuestionsReply::Settled(Settlement::Settled))
    );
    assert_eq!(
        native.questions(dismiss.clone()).await.result,
        Ok(QuestionsReply::Settled(Settlement::Stale)),
        "a write naming a version that changed since is stale"
    );

    let list = QuestionsQuery::List {
        project: project.clone(),
    };
    let Ok(QuestionsReply::Page(page)) = native.questions(list.clone()).await.result else {
        panic!("the list is read through the engine");
    };
    let pending: Vec<u64> = page
        .items
        .iter()
        .filter(|question| question.status == QuestionStatus::Pending)
        .map(|question| question.id)
        .collect();
    assert_eq!(
        pending, ids,
        "every pending question, oldest first, past the first page"
    );
    assert!(
        page.items
            .iter()
            .any(|question| question.id == settled && question.status == QuestionStatus::Dismissed),
        "the settled one after them"
    );
    assert!(
        page.items
            .iter()
            .skip_while(|question| question.status == QuestionStatus::Pending)
            .all(|question| question.status != QuestionStatus::Pending)
    );

    let answer = QuestionsQuery::Answer {
        project: project.clone(),
        id: ids[0],
        version: 0,
        answer: Answer {
            picks: vec![1],
            comment: None,
        },
    };
    assert_eq!(
        native.questions(answer).await.result,
        Ok(QuestionsReply::Settled(Settlement::Settled))
    );
    assert_eq!(
        native.questions(QuestionsQuery::Counts).await.result,
        Ok(QuestionsReply::Counts(vec![(
            project.clone(),
            u32::try_from(ids.len() - 1).unwrap()
        )]))
    );

    native.detach();
    assert_eq!(
        native.questions(list.clone()).await.result,
        Err("trusted_connection_unavailable".into()),
        "a detached worker answers for its own query"
    );
}
