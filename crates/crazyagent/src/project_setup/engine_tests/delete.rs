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
async fn a_delete_plan_is_gone_when_the_reply_arrives_even_after_a_settle_that_failed() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    std::fs::remove_dir_all(&folder).unwrap();
    write_db(
        &engine,
        "CREATE TABLE settle_fault(n);
         CREATE TRIGGER settle_fails_once BEFORE DELETE ON projects_delete_pending
         WHEN (SELECT count(*) FROM settle_fault) = 0
         BEGIN
           INSERT INTO settle_fault VALUES (1);
           SELECT RAISE(FAIL, 'settle fails once');
         END;",
    );
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects_delete_pending"),
        0,
        "the reply means the plan is gone, or every home move stays refused until a restart"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM settle_fault"),
        1,
        "the first settle really failed"
    );
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
async fn delete_everything_removes_the_projects_master_lands_and_catch_up_choices() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    write_db(
        &engine,
        "INSERT INTO team_master_lands(project,mode) VALUES ('setup-pooled','waits'),('other','waits');
         INSERT INTO team_catch_up_main(project,mode) VALUES ('setup-pooled','off'),('other','off');",
    );
    std::fs::remove_dir_all(&folder).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    for table in ["team_master_lands", "team_catch_up_main"] {
        assert_eq!(
            engine.rows(&format!(
                "SELECT count(*) FROM {table} WHERE project='setup-pooled'"
            )),
            0,
            "{table} keeps the deleted project's row"
        );
        assert_eq!(
            engine.rows(&format!(
                "SELECT count(*) FROM {table} WHERE project='other'"
            )),
            1,
            "{table} loses another project's row"
        );
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

/// Sets up a second project, Service A, beside the pooled one. Inputs: the engine's owner
/// client and the generation. Output: the project and its folder.
async fn service(owner: &mut OwnerController, generation: u64) -> (ProjectId, PathBuf) {
    let project = ProjectId::new("service-a").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Service A"),
            pooled_setup(vec![]),
            generation,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    (project, stored.project_folder())
}

fn set(engine: &Engine, sql: &str, parameters: impl rusqlite::Params) {
    rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute(sql, parameters)
        .unwrap();
}

/// Delete everything never moves a folder another project uses: with Service A's folder
/// inside the pooled project's folder, neither project offers a delete, the preview names
/// the other project, and a Delete of either refuses `delete_shared_folder` with nothing
/// stopped or moved.
#[tokio::test]
async fn delete_everything_refuses_a_folder_that_holds_another_projects_folder() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let (service, _) = service(&mut owner, 2).await;
    let nested = folder.join("service-a");
    std::fs::create_dir(&nested).unwrap();
    set(
        &engine,
        "UPDATE project_setups SET body=json_set(body,'$.folder',?1) WHERE project='service-a'",
        [nested.to_str().unwrap()],
    );
    for (id, other) in [(&project, "Service A"), (&service, NAME)] {
        let preview = owner.removal_preview(id.clone()).await.unwrap();
        assert!(!preview.can_delete, "{preview:?}");
        assert!(
            preview
                .blocked
                .as_deref()
                .is_some_and(|why| why.contains(other)),
            "the preview names {other}: {preview:?}"
        );
    }
    for (id, name, generation) in [(&project, NAME, 3), (&service, "Service A", 4)] {
        let response = owner
            .delete_project(id.clone(), name.into(), generation)
            .await
            .unwrap();
        assert_eq!(result(response), rejected("delete_shared_folder"), "{name}");
    }
    assert_eq!(kept(&engine), 2);
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects WHERE id='service-a'"),
        1
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM team_paused_projects"),
        0,
        "a refused delete stops no agent"
    );
    assert!(folder.is_dir() && nested.is_dir());
    drop(owner);
    engine.stop().await;
}

/// Two projects that declare the same repository inside the home: Delete everything of
/// one would move the other's repository, so the preview offers no delete and names the
/// other project, and a Delete refuses `delete_shared_folder` with nothing stopped or moved.
#[tokio::test]
async fn delete_everything_refuses_a_repository_another_project_declares() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let (_, service_folder) = service(&mut owner, 2).await;
    let shared = engine.home().join("shared-lib");
    std::fs::create_dir(&shared).unwrap();
    for id in [PROJECT, "service-a"] {
        set(
            &engine,
            "UPDATE projects SET body=json_set(body,'$.spec.repositories',json_array(?1)) WHERE id=?2",
            [shared.to_str().unwrap(), id],
        );
    }
    let preview = owner.removal_preview(project.clone()).await.unwrap();
    assert!(!preview.can_delete, "{preview:?}");
    assert!(
        preview
            .blocked
            .as_deref()
            .is_some_and(|why| why.contains("Service A")),
        "{preview:?}"
    );
    let response = owner
        .delete_project(project.clone(), NAME.into(), 3)
        .await
        .unwrap();
    assert_eq!(result(response), rejected("delete_shared_folder"));
    assert_eq!(kept(&engine), 2);
    assert_eq!(paused(&engine), 0, "a refused delete stops no agent");
    assert!(folder.is_dir() && shared.is_dir() && service_folder.is_dir());
    drop(owner);
    engine.stop().await;
}

/// A projects home move and a Delete everything never overlap: a pending move refuses a
/// delete with nothing stopped, and a move asked while a delete waits for the project's
/// agents refuses `home_move_delete_in_progress`; the delete's plan is forgotten once it ends.
#[tokio::test]
async fn a_pending_home_move_holds_a_delete_and_a_planned_delete_holds_a_home_move() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    let home = engine.home();
    let next = tempfile::Builder::new()
        .prefix("ca-next-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let next = std::fs::canonicalize(next.path()).unwrap();
    set(
        &engine,
        "INSERT INTO projects_home_move VALUES (1,'request-7',7,?1,?2,'[\"meal-planner\"]')",
        [home.to_str().unwrap(), next.to_str().unwrap()],
    );
    let preview = owner.removal_preview(project.clone()).await.unwrap();
    assert!(!preview.can_delete, "{preview:?}");
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), rejected("home_move_in_progress"));
    assert_eq!(paused(&engine), 0, "a refused delete stops no agent");

    write_db(
        &engine,
        "DELETE FROM projects_home_move;
         INSERT INTO team_paused_projects(project,since_ms) VALUES ('setup-pooled',1);
         INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (7,'setup-pooled','sub','codex',1,'working',0,0);",
    );
    let mut desktop =
        OwnerController::attach(engine.root.path(), OwnerClientKind::Desktop, ENGINE_WAIT)
            .await
            .unwrap();
    let planned = || engine.rows("SELECT count(*) FROM projects_delete_pending");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let deleting = owner.delete_project(project.clone(), NAME.into(), 3);
    let moving = async {
        let deadline = std::time::Instant::now() + ENGINE_WAIT;
        while planned() == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = planned() == 1;
        let moved = if seen {
            Some(
                desktop
                    .move_projects_home(next.to_str().unwrap().into(), true, 1)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        write_db(
            &engine,
            "UPDATE team_agents SET status='stopped' WHERE id=7",
        );
        (seen, moved)
    };
    let (deleted, (seen, moved)) = tokio::join!(deleting, moving);
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        seen,
        "the delete records its plan before it waits for the agents"
    );
    assert_eq!(
        result(moved.unwrap()),
        rejected("home_move_delete_in_progress")
    );
    assert_eq!(
        result(deleted.unwrap()),
        rejected("home_trash_failed"),
        "the read-only home keeps the folder out of the Trash"
    );
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while planned() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(planned(), 0, "the plan is forgotten once the delete ends");
    assert!(folder.is_dir());
    assert_eq!(kept(&engine), 2);
    drop(desktop);
    drop(owner);
    engine.stop().await;
}
