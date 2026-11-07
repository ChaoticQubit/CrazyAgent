use super::*;

const PROJECT: &str = "setup-pooled";
const NAME: &str = "Meal planner";

/// Sets up the one pooled project of a test engine. Inputs: the engine's owner client.
/// Output: the project and its folder.
async fn project(owner: &mut OwnerController) -> (ProjectId, PathBuf) {
    let project = ProjectId::new(PROJECT).unwrap();
    let response = owner
        .setup_project(project.clone(), spec(NAME), pooled_setup(vec![]), 1)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    (project, stored.project_folder())
}

fn write_db(engine: &Engine, sql: &str) {
    rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

fn kept(engine: &Engine) -> i64 {
    engine.rows("SELECT count(*) FROM projects WHERE id='setup-pooled'")
        + engine.rows("SELECT count(*) FROM project_setups WHERE project='setup-pooled'")
}

fn paused(engine: &Engine) -> i64 {
    engine.rows("SELECT count(*) FROM team_paused_projects WHERE project='setup-pooled'")
}

#[tokio::test]
async fn delete_everything_refuses_a_name_that_is_not_exactly_the_projects_name() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    for (generation, typed) in [(2, "Meal planner "), (3, "meal planner"), (4, "")] {
        let response = owner
            .delete_project(project.clone(), typed.into(), generation)
            .await
            .unwrap();
        assert_eq!(
            result(response),
            rejected("delete_name_mismatch"),
            "{typed:?}"
        );
    }
    assert_eq!(kept(&engine), 2, "the project stays listed");
    assert_eq!(paused(&engine), 0, "a refused name stops no agent");
    assert!(folder.is_dir());
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn a_project_with_a_repository_outside_the_home_offers_no_delete_and_refuses_one() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let outside = tempfile::Builder::new()
        .prefix("ca-outside-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let outside_path = std::fs::canonicalize(outside.path()).unwrap();
    rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute(
            "UPDATE projects SET body=json_set(body,'$.spec.repositories',json_array(?1,?2)) WHERE id=?3",
            rusqlite::params![
                folder.to_str().unwrap(),
                outside_path.to_str().unwrap(),
                PROJECT
            ],
        )
        .unwrap();
    let preview = owner.removal_preview(project.clone()).await.unwrap();
    assert!(!preview.can_delete, "{preview:?}");
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), rejected("delete_outside_home"));
    assert_eq!(kept(&engine), 2);
    assert_eq!(paused(&engine), 0, "a refused folder stops no agent");
    assert!(folder.is_dir() && outside_path.is_dir());
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn a_trash_move_that_fails_removes_nothing_and_leaves_stop_all_on() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let home = engine.home();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result(response), rejected("home_trash_failed"));
    assert_eq!(kept(&engine), 2, "the records stay when the move fails");
    assert!(
        folder.join(".crazyagent/context/brief.md").is_file(),
        "the folder stays where it was"
    );
    assert_eq!(
        paused(&engine),
        1,
        "the agents stay stopped after a failed move"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn delete_everything_of_a_project_whose_folder_is_gone_removes_its_records_and_engine_files()
{
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    write_db(
        &engine,
        "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (9,'setup-pooled','sub','codex',1,'stopped',0,0),
                (10,'other','sub','codex',1,'stopped',0,0);
         INSERT INTO research_jobs(id,project,kind,harness,tools,status,created_ms)
         VALUES (41,'setup-pooled','market_research','codex',1,'done',0),
                (42,'other','market_research','codex',1,'done',0);",
    );
    let root = engine.root.path();
    let ours = [
        root.join("team/agents/agent-9/home"),
        root.join("research/agents/job-41-1/home"),
    ];
    let theirs = [
        root.join("team/agents/agent-10/home"),
        root.join("research/agents/job-42-1/home"),
    ];
    for path in ours.iter().chain(&theirs) {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::remove_dir_all(&folder).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert_eq!(
        kept(&engine),
        0,
        "a missing folder still removes the records"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM team_agents WHERE project='setup-pooled'"),
        0
    );
    for path in &ours {
        assert!(
            !path.parent().unwrap().exists(),
            "{} goes with the project",
            path.display()
        );
    }
    for path in &theirs {
        assert!(path.is_dir(), "{} stays", path.display());
    }
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn removing_a_project_keeps_its_usage_under_removed_projects() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    drop(crate::usage::open(engine.root.path()).unwrap());
    write_db(
        &engine,
        "INSERT INTO usage_tokens VALUES
           (3600000,'agent','setup-pooled','pool-team','account-codex','1','codex','s1',10,20,30,40),
           (7200000,'master','setup-pooled','pool-team','account-codex','m','codex','s2',1,1,1,1),
           (3600000,'agent','<removed>','pool-team','account-codex','1','codex','s1',100,200,300,400),
           (3600000,'agent','other','pool-team','account-codex','1','codex','s1',5,5,5,5);
         INSERT INTO usage_unreported VALUES (3600000,'agent','setup-pooled','1','rcli',5);
         INSERT INTO usage_seen VALUES ('setup-pooled','1','s1',5);
         INSERT INTO usage_runs(home,kind,project,account,agent,harness,started_ms)
         VALUES ('/agent-home-1','agent','setup-pooled','account-codex','1','codex',5);
         INSERT INTO usage_cut VALUES ('/agent-home-1','setup-pooled',5);",
    );
    let totals = |engine: &Engine| {
        engine.rows(
            "SELECT SUM(input)*1000000+SUM(output)*10000+SUM(cache_read)*100+SUM(cache_write) FROM usage_tokens",
        )
    };
    let before = totals(&engine);
    let removed = owner.remove_project(project.clone(), 2).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert!(folder.is_dir(), "Remove from dashboard keeps the folder");
    assert_eq!(totals(&engine), before, "no token is lost or counted twice");
    for table in [
        "usage_tokens",
        "usage_unreported",
        "usage_seen",
        "usage_runs",
        "usage_cut",
    ] {
        assert_eq!(
            engine.rows(&format!(
                "SELECT count(*) FROM {table} WHERE project='setup-pooled'"
            )),
            0,
            "{table} keeps nothing under the old id"
        );
    }
    assert_eq!(
        engine.rows(
            "SELECT input*1000+output FROM usage_tokens WHERE project='<removed>' AND hour_ms=3600000"
        ),
        110_220,
        "the same hour and run adds into the Removed projects row"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM usage_tokens WHERE project='<removed>'"),
        2
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM usage_tokens WHERE project='other'"),
        1
    );
    for table in ["usage_unreported", "usage_seen", "usage_runs", "usage_cut"] {
        assert_eq!(
            engine.rows(&format!(
                "SELECT count(*) FROM {table} WHERE project='<removed>'"
            )),
            1,
            "{table} moves to Removed projects"
        );
    }
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn the_removal_preview_names_the_project_its_folder_and_offers_delete_inside_the_home() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let preview = owner.removal_preview(project.clone()).await.unwrap();
    assert_eq!(preview.name, NAME);
    assert_eq!(preview.folders, [folder.to_str().unwrap().to_owned()]);
    assert!(preview.can_delete);
    assert!(preview.running.is_empty(), "{preview:?}");
    assert_eq!(preview.open_change_sets, 0);
    assert!(preview.losses.is_empty());
    assert_eq!(kept(&engine), 2, "a preview changes nothing");
    assert_eq!(paused(&engine), 0);
    drop(owner);
    engine.stop().await;
}
