use super::*;
use crate::engine::bootstrap::EngineService;
use crate::project_setup::{StoredSetup, TeamPlan};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn private_root(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn set_up_project(root: &Path, folder: &Path) -> ProjectId {
    let project = ProjectId::new("process-1").unwrap();
    let setup = StoredSetup {
        project_id: project.clone(),
        name: "Process".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1,
        revision: 1,
    };
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            [
                project.as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            ],
        )
        .unwrap();
    project
}

#[test]
fn a_request_without_a_trusted_connection_answers_for_its_own_query() {
    let query = ProcessQuery::Save {
        project: None,
        text: "rule".into(),
    };
    let event = ProcessEvent::unavailable(query.clone());
    assert_eq!(event.query, query);
    assert_eq!(event.result, Err("trusted_connection_unavailable".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_saves_both_texts_through_the_engine_and_every_launch_reads_them() {
    let root = private_root("ca-tprocess-");
    let work = private_root("ca-tprocess-work-");
    let folder = std::fs::canonicalize(work.path()).unwrap().join("project");
    std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();

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
    let project = set_up_project(root.path(), &folder);

    let read = |project: Option<ProjectId>| ProcessQuery::Read { project };
    let event = native.process(read(None)).await;
    assert_eq!(event.query, read(None));
    assert_eq!(event.result, Ok(ProcessReply::Text(String::new())));
    for (scope, text) in [
        (None, "APP-RULE-51\n"),
        (Some(project.clone()), "PROJECT-RULE-52\n"),
    ] {
        let save = ProcessQuery::Save {
            project: scope.clone(),
            text: text.into(),
        };
        assert_eq!(
            native.process(save).await.result,
            Ok(ProcessReply::Text(String::new()))
        );
        assert_eq!(
            native.process(read(scope)).await.result,
            Ok(ProcessReply::Text(text.into()))
        );
    }
    assert_eq!(
        std::fs::read_to_string(folder.join(".crazyagent/instructions.md")).unwrap(),
        "PROJECT-RULE-52\n"
    );

    let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    let launch = crate::launch_context::agent_project(&tx, &project).unwrap();
    let given = crate::agent_home::text(&launch).unwrap();
    let app = given
        .find("APP-RULE-51")
        .expect("a launch reads the saved app text");
    let own = given
        .find("PROJECT-RULE-52")
        .expect("a launch reads the saved project text");
    assert!(app < own);
    drop(tx);

    crate::team::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
             VALUES (?1, 'sub', 'codex', 0, 'stopped', 1, 1)",
            [project.as_str()],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    let asked = ProcessQuery::Agent {
        project: project.clone(),
        agent: Some(agent),
    };
    let Ok(ProcessReply::Agent(view)) = native.process(asked.clone()).await.result else {
        panic!("an agent before its first launch is still shown");
    };
    assert!(!view.launched);
    let home = crate::team::runtime::agent_home(&root.path().join("team"), agent);
    std::fs::create_dir_all(&home).unwrap();
    for folder in [home.parent().unwrap(), home.as_path()] {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    crate::agent_home::prepare(&home, crate::agent_home::Harness::Codex, &launch, &given).unwrap();
    crate::agent_home::record_skills(&home, &[]).unwrap();
    let Ok(ProcessReply::Agent(view)) = native.process(asked.clone()).await.result else {
        panic!("the launched agent's view is read through the engine");
    };
    assert_eq!(view.agent, agent);
    assert_eq!(view.harness, "codex");
    assert!(view.launched);
    assert_eq!(
        view.text, given,
        "the view shows exactly what the launch wrote"
    );
    assert!(view.skills.is_empty(), "a launch that installed no skill records none");
    std::fs::remove_file(home.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(
        folder.join(".crazyagent/instructions.md"),
        home.join("AGENTS.md"),
    )
    .unwrap();
    assert_eq!(
        native.process(asked).await.result,
        Err("agent_instructions_unsafe".into()),
        "a linked file in the agent home is refused"
    );

    let event = native.process(ProcessQuery::Default).await;
    assert_eq!(
        event.result,
        Ok(ProcessReply::Text(
            crate::agent_home::DEFAULT_PROCESS.into()
        ))
    );
    let unknown = ProjectId::new("not-set-up").unwrap();
    assert_eq!(
        native.process(read(Some(unknown))).await.result,
        Err("process_project_unknown".into())
    );
    let over = ProcessQuery::Save {
        project: None,
        text: "a".repeat(crate::agent_home::TEXT_BYTES_MAX + 1),
    };
    assert_eq!(
        native.process(over).await.result,
        Err("process_text_too_large".into())
    );
    assert_eq!(
        native.process(read(None)).await.result,
        Ok(ProcessReply::Text("APP-RULE-51\n".into())),
        "a refused save keeps the saved text"
    );
    service.shutdown().await.unwrap();
}
