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

/// Delete everything of a project whose folder is already gone removes its records, then its
/// engine files once the reply is out, and leaves another project's files alone.
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
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while ours.iter().any(|path| path.parent().unwrap().exists())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
         INSERT INTO team_catch_up_main(project,mode) VALUES ('setup-pooled','off'),('other','off');
         INSERT INTO team_reuse_idle(project,mode) VALUES ('setup-pooled','on'),('other','on');",
    );
    std::fs::remove_dir_all(&folder).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    for table in ["team_master_lands", "team_catch_up_main", "team_reuse_idle"] {
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
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,input,output,cache_read,cache_write) VALUES
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
/// agents refuses `home_move_delete_in_progress`; the delete's plan is forgotten once it ends, even refused, and the next move goes.
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
    let moved = desktop
        .move_projects_home(next.to_str().unwrap().into(), true, 2)
        .await
        .unwrap();
    assert_eq!(
        result(moved),
        OwnerResult::Applied,
        "a delete refused after its plan was written holds no later move"
    );
    drop(desktop);
    drop(owner);
    engine.stop().await;
}

/// While a Delete everything waits, the owner can neither pick another projects home nor move
/// it without its projects, so the delete never trashes folders from a home the owner has
/// left. Both go once the plan is forgotten.
#[tokio::test]
async fn a_planned_delete_holds_a_home_switch_until_it_is_forgotten() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    project(&mut owner).await;
    let next = tempfile::Builder::new()
        .prefix("ca-next-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let saved = || {
        engine.rows(&format!(
            "SELECT count(*) FROM projects_home WHERE path='{}'",
            engine.home().display()
        ))
    };
    write_db(
        &engine,
        "INSERT INTO projects_delete_pending(exchange) VALUES (7)",
    );
    let refused = rejected("home_move_delete_in_progress");
    let set = owner.set_projects_home(target.clone(), 2).await.unwrap();
    assert_eq!(result(set), refused);
    let moved = owner
        .move_projects_home(target.clone(), false, 3)
        .await
        .unwrap();
    assert_eq!(result(moved), refused);
    assert_eq!(saved(), 1, "the refusals leave the home the plan saw");
    write_db(&engine, "DELETE FROM projects_delete_pending");
    let set = owner.set_projects_home(target, 4).await.unwrap();
    assert_eq!(result(set), OwnerResult::Applied);
    assert_eq!(saved(), 0, "the switch goes once the plan is forgotten");
    drop(owner);
    engine.stop().await;
}

/// An engine start forgets a delete plan a crash left behind, so nothing holds a home move.
#[tokio::test]
async fn an_engine_start_forgets_a_delete_plan_left_behind() {
    let mut engine = Engine::start().await;
    write_db(
        &engine,
        "INSERT INTO projects_delete_pending(exchange) VALUES (7)",
    );
    engine.restart().await;
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects_delete_pending"),
        0
    );
    engine.stop().await;
}

/// While a projects home move is pending, the owner can neither pick another home nor move it
/// without its projects, so the move's own pick is never overwritten. Both go once the move
/// is forgotten.
#[tokio::test]
async fn a_pending_home_move_holds_a_home_switch_until_it_is_forgotten() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    project(&mut owner).await;
    let next = tempfile::Builder::new()
        .prefix("ca-next-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let target = std::fs::canonicalize(next.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let home = engine.home();
    set(
        &engine,
        "INSERT INTO projects_home_move VALUES (1,'request-7',7,?1,?2,'[\"meal-planner\"]')",
        [home.to_str().unwrap(), target.as_str()],
    );
    let refused = rejected("home_move_in_progress");
    let picked = owner.set_projects_home(target.clone(), 2).await.unwrap();
    assert_eq!(result(picked), refused);
    let moved = owner
        .move_projects_home(target.clone(), false, 3)
        .await
        .unwrap();
    assert_eq!(result(moved), refused);
    write_db(&engine, "DELETE FROM projects_home_move");
    let picked = owner.set_projects_home(target, 4).await.unwrap();
    assert_eq!(result(picked), OwnerResult::Applied);
    drop(owner);
    engine.stop().await;
}

/// A projects home move whose record and first settle both fail is finished by a later try,
/// so its row does not refuse every home change until a restart. The failures last 90 ms from
/// the plan's write, which is less than the two gaps before the third try.
#[tokio::test]
async fn a_home_move_is_finished_when_its_record_and_first_settle_fail() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    project(&mut owner).await;
    let mut desktop =
        OwnerController::attach(engine.root.path(), OwnerClientKind::Desktop, ENGINE_WAIT)
            .await
            .unwrap();
    let scratch_home = || {
        let dir = tempfile::Builder::new()
            .prefix("ca-next-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let path = std::fs::canonicalize(dir.path())
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        (dir, path)
    };
    let (_next, next_path) = scratch_home();
    let (_other, other_path) = scratch_home();
    write_db(
        &engine,
        "CREATE TABLE settle_gate(until REAL);
         CREATE TRIGGER settle_gate_opens AFTER INSERT ON projects_home_move
         BEGIN
           DELETE FROM settle_gate;
           INSERT INTO settle_gate VALUES ((julianday('now') - 2440587.5) * 86400.0 + 0.09);
         END;
         CREATE TRIGGER settle_fails_at_first BEFORE DELETE ON projects_home_move
         WHEN (julianday('now') - 2440587.5) * 86400.0 < (SELECT until FROM settle_gate)
         BEGIN
           SELECT RAISE(FAIL, 'settle fails at first');
         END;",
    );
    let _ = desktop.move_projects_home(next_path, true, 1).await;
    let waiting = || engine.rows("SELECT count(*) FROM projects_home_move");
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while waiting() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(waiting(), 0, "a later settle finishes the move");
    let picked = owner.set_projects_home(other_path, 2).await.unwrap();
    assert_eq!(
        result(picked),
        OwnerResult::Applied,
        "a finished move holds no later home change"
    );
    drop(desktop);
    drop(owner);
    engine.stop().await;
}

/// When every settle before the reply fails the reply still goes, and the settle after it
/// forgets the plan, so a home move is not refused until a restart.
#[tokio::test]
async fn a_delete_plan_is_forgotten_after_the_reply_when_every_settle_before_it_failed() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    std::fs::remove_dir_all(&folder).unwrap();
    write_db(
        &engine,
        "CREATE TABLE settle_fault(n);
         CREATE TRIGGER settle_fails_thrice BEFORE DELETE ON projects_delete_pending
         WHEN (SELECT count(*) FROM settle_fault) < 3
         BEGIN
           INSERT INTO settle_fault VALUES (1);
           SELECT RAISE(FAIL, 'settle fails');
         END;",
    );
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let planned = || engine.rows("SELECT count(*) FROM projects_delete_pending");
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while planned() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(planned(), 0, "the settle after the reply forgets the plan");
    assert_eq!(
        engine.rows("SELECT count(*) FROM settle_fault"),
        3,
        "every settle before the reply really failed"
    );
    drop(owner);
    engine.stop().await;
}

/// Makes an agent's folder and profile files and a research run on disk, as a project's
/// engine files. Inputs: the engine root, the agent and the run's folder name. Output: every
/// path made.
fn engine_files(root: &std::path::Path, agent: i64, run: &str) -> Vec<PathBuf> {
    let team = root.join("team");
    let profiles = team.join("profiles");
    let folders = [
        team.join("agents").join(format!("agent-{agent}")),
        root.join("research").join("agents").join(run),
    ];
    for folder in &folders {
        std::fs::create_dir_all(folder.join("home")).unwrap();
    }
    std::fs::create_dir_all(&profiles).unwrap();
    let files = [
        format!("agent-{agent}.sb"),
        format!("agent-{agent}-teammates.sb"),
        format!("agent-{agent}-teammates-setup.sb"),
        format!("agent-{agent}-teammates.socket"),
        format!("agent-{agent}-teammates-server.pid"),
    ]
    .map(|name| profiles.join(name));
    for file in &files {
        std::fs::write(file, "kept until a record names it").unwrap();
    }
    folders.into_iter().chain(files).collect()
}

/// Writes the rows of a finished agent and a finished research job of the test project, each
/// with no process behind it. Input: the engine.
fn engine_file_rows(engine: &Engine) {
    write_db(
        engine,
        "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (9,'setup-pooled','sub','codex',1,'stopped',0,0);
         INSERT INTO research_jobs(id,project,kind,harness,tools,status,created_ms)
         VALUES (41,'setup-pooled','market_research','codex',1,'done',0);",
    );
}

fn recorded(engine: &Engine) -> i64 {
    engine.rows("SELECT count(*) FROM projects_delete_files")
}

/// Reads a folder's inode as a delete's record keeps it. Input: the folder. Output: the number.
fn inode(folder: &std::path::Path) -> i64 {
    let metadata = std::fs::symlink_metadata(folder).unwrap();
    i64::try_from(std::os::unix::fs::MetadataExt::ino(&metadata)).unwrap()
}

/// Writes one record as a delete whose reply never arrived leaves it. Inputs: the engine, the
/// name and the inode.
fn record(engine: &Engine, name: &str, inode: i64) {
    write_db(
        engine,
        &format!("INSERT INTO projects_delete_files(name,inode) VALUES ('{name}',{inode})"),
    );
}

/// Waits for a start to settle every record, the last step of its removal. Input: the engine.
async fn settled(engine: &Engine) {
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while recorded(engine) != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(recorded(engine), 0, "a start settles every record");
}

/// A delete whose reply never arrived leaves its record behind, so the next engine start
/// removes exactly the files it names and clears it.
#[tokio::test]
async fn an_engine_start_finishes_the_files_a_delete_recorded() {
    let mut engine = Engine::start().await;
    let made = engine_files(engine.root.path(), 9, "job-41-1");
    record(&engine, "agent-9", inode(&made[0]));
    record(&engine, "job-41-1", inode(&made[1]));
    engine.restart().await;
    settled(&engine).await;
    for path in &made {
        assert!(!path.exists(), "{} goes", path.display());
    }
    engine.stop().await;
}

/// Absence is not intent: a start removes nothing a record does not name exactly, so a
/// folder with no row and no record survives it, and a name shaped like a path is dropped
/// unused.
#[tokio::test]
async fn an_engine_start_keeps_every_engine_file_no_record_names_exactly() {
    let mut engine = Engine::start().await;
    let root = engine.root.path().to_path_buf();
    let made = engine_files(&root, 77, "job-88-1");
    let reached = [
        ("../agents/agent-77", made[0].clone()),
        ("job-88-1/home", made[1].join("home")),
        (
            "agent-077",
            root.join("team").join("agents").join("agent-077"),
        ),
        (
            "job-88",
            root.join("research").join("agents").join("job-88"),
        ),
    ];
    for (name, folder) in &reached {
        std::fs::create_dir_all(folder).unwrap();
        record(&engine, name, inode(folder));
    }
    engine.restart().await;
    settled(&engine).await;
    for path in made.iter().chain(reached.iter().map(|(_, folder)| folder)) {
        assert!(path.exists(), "{} stays", path.display());
    }
    engine.stop().await;
}

/// A removal that stops on one of an agent's small files leaves its folder, so the record
/// still matches it and the next start finishes every file.
#[tokio::test]
async fn a_start_stopped_by_one_file_keeps_the_folder_so_the_next_start_finishes() {
    let mut engine = Engine::start().await;
    let root = engine.root.path().to_path_buf();
    let made = engine_files(&root, 9, "job-41-1");
    let planted = root
        .join("team")
        .join("profiles")
        .join("agent-9-teammates-server.pid");
    std::fs::remove_file(&planted).unwrap();
    std::fs::create_dir(&planted).unwrap();
    record(&engine, "agent-9", inode(&made[0]));
    record(&engine, "job-41-1", inode(&made[1]));
    engine.restart().await;
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while recorded(&engine) != 1 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        recorded(&engine),
        1,
        "only the stopped removal keeps its record"
    );
    assert!(
        made[0].exists(),
        "the folder goes last, so a retry still matches"
    );
    std::fs::remove_dir(&planted).unwrap();
    engine.restart().await;
    settled(&engine).await;
    for path in &made {
        assert!(!path.exists(), "{} goes", path.display());
    }
    engine.stop().await;
}

/// A delete records its engine files with the rows it drops and removes every one, the
/// lead's teammate files too, so only a failed clear leaves the record behind.
#[tokio::test]
async fn a_delete_records_its_engine_files_and_removes_every_one() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    engine_file_rows(&engine);
    write_db(
        &engine,
        "CREATE TRIGGER record_stays BEFORE DELETE ON projects_delete_files
         BEGIN SELECT RAISE(FAIL, 'record stays'); END;",
    );
    let made = engine_files(engine.root.path(), 9, "job-41-1");
    std::fs::remove_dir_all(&folder).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while made.iter().any(|path| path.exists()) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for path in &made {
        assert!(!path.exists(), "{} goes", path.display());
    }
    assert_eq!(
        engine.rows(
            "SELECT count(*) FROM projects_delete_files WHERE name IN ('agent-9','job-41-1')"
        ),
        2
    );
    assert_eq!(recorded(&engine), 2, "the record names nothing else");
    drop(owner);
    engine.stop().await;
}

/// An old database image put back can name an id another database has since used, so a
/// record removes only the folder it was written for: another folder under the recorded name
/// stays, a name whose folder is gone removes nothing, and the record clears either way.
#[tokio::test]
async fn an_engine_start_keeps_a_folder_its_record_s_inode_does_not_name() {
    let mut engine = Engine::start().await;
    let first = engine_files(engine.root.path(), 9, "job-41-1");
    let (agent, run) = (inode(&first[0]), inode(&first[1]));
    let aside = engine.root.path().join("aside");
    std::fs::create_dir_all(&aside).unwrap();
    std::fs::rename(&first[0], aside.join("agent")).unwrap();
    std::fs::rename(&first[1], aside.join("run")).unwrap();
    let made = engine_files(engine.root.path(), 9, "job-41-1");
    assert_ne!(inode(&made[0]), agent, "another agent folder, same name");
    assert_ne!(inode(&made[1]), run, "another run folder, same name");
    record(&engine, "agent-9", agent);
    record(&engine, "job-41-1", run);
    record(&engine, "agent-12", agent);
    engine.restart().await;
    settled(&engine).await;
    for path in &made {
        assert!(path.exists(), "{} stays", path.display());
    }
    engine.stop().await;
}

/// A delete refused after its plan was written drops no rows, so it records nothing and a
/// later start removes nothing.
#[tokio::test]
async fn a_delete_refused_after_its_plan_records_and_removes_nothing() {
    let mut engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    engine_file_rows(&engine);
    let made = engine_files(engine.root.path(), 9, "job-41-1");
    let home = engine.home();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let response = owner
        .delete_project(project.clone(), NAME.into(), 2)
        .await
        .unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(result(response), rejected("home_trash_failed"));
    assert_eq!(recorded(&engine), 0);
    drop(owner);
    engine.restart().await;
    for path in &made {
        assert!(path.exists(), "{} stays", path.display());
    }
    assert!(folder.is_dir());
    engine.stop().await;
}

/// A replayed delete records and removes nothing twice: files that appear under the same
/// names after the first delete stay through the replay and a start.
#[tokio::test]
async fn a_replayed_delete_records_and_removes_nothing_twice() {
    let mut engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (project, folder) = project(&mut owner).await;
    engine_file_rows(&engine);
    let made = engine_files(engine.root.path(), 9, "job-41-1");
    std::fs::remove_dir_all(&folder).unwrap();
    let revision = engine.rows("SELECT revision FROM state WHERE id=1");
    let request = OwnerRequest {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        request_id: crate::protocol::RequestId::new("delete-replay-1".to_owned()).unwrap(),
        expected_revision: crate::protocol::Cursor::new(u64::try_from(revision).unwrap()),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::DeleteProject {
                project_id: project.clone(),
                name: NAME.into(),
            },
        },
    };
    let client = owner.shared_client().unwrap();
    assert_eq!(
        result(client.request(request.clone()).await.unwrap().response),
        OwnerResult::Applied
    );
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while (made[0].exists() || recorded(&engine) != 0) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!made[0].exists(), "the first delete removes the files");
    assert_eq!(recorded(&engine), 0, "the first delete clears its record");
    let again = engine_files(engine.root.path(), 9, "job-41-1");
    let replayed = client.request(request).await.unwrap().response;
    assert!(
        matches!(&replayed, OwnerResponse::Applied { operation } if operation.historical_replay),
        "{replayed:?}"
    );
    assert_eq!(recorded(&engine), 0, "a replay records nothing");
    drop(client);
    drop(owner);
    engine.restart().await;
    for path in &again {
        assert!(path.exists(), "{} stays through the replay", path.display());
    }
    engine.stop().await;
}

/// T8 r1 secreview L2: Remove from dashboard takes the tool cache of the project's folder, so
/// a different project added later at that folder starts with none, and keeps the cache of a
/// repository another project still declares, whose agents use it; the record of what it
/// takes is cleared once the cache is gone.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn removing_a_project_takes_its_tool_cache_and_keeps_one_another_project_uses() {
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
    let cache = |of: &std::path::Path| {
        crate::sandbox::tool_cache::project_folder(engine.root.path(), of.to_str().unwrap())
    };
    for of in [&folder, &shared] {
        std::fs::create_dir_all(cache(of).join("sandboxed-worktree").join("uv")).unwrap();
    }
    let removed = owner.remove_project(project.clone(), 3).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert!(folder.is_dir(), "Remove from dashboard keeps the folder");
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while (cache(&folder).exists() || engine.rows("SELECT count(*) FROM projects_delete_files") > 0)
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !cache(&folder).exists(),
        "the project's own tool cache goes with it"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects_delete_files"),
        0,
        "the record is cleared once the cache is gone"
    );
    assert!(
        cache(&shared).is_dir(),
        "Service A still uses the shared repository's cache"
    );
    assert!(service_folder.is_dir() && shared.is_dir());
    drop(owner);
    engine.stop().await;
}

/// T8-R2-N3: a tool cache record an earlier Remove from dashboard left unfinished, for a
/// folder a project added again since names, never takes that project's cache: the next
/// finish, here after another project's removal, checks the projects again, keeps the cache
/// its agents use and clears the stale record, and still takes the removed project's cache.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_stale_tool_cache_record_never_takes_the_cache_of_a_project_added_again() {
    use std::os::unix::fs::MetadataExt;
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let (_, folder) = project(&mut owner).await;
    let (service, service_folder) = service(&mut owner, 2).await;
    let cache = |of: &std::path::Path| {
        crate::sandbox::tool_cache::project_folder(engine.root.path(), of.to_str().unwrap())
    };
    for of in [&folder, &service_folder] {
        std::fs::create_dir_all(cache(of).join("sandboxed-worktree").join("uv")).unwrap();
    }
    let stale = format!(
        "{}{}",
        crate::projects_home::TOOL_CACHE_PREFIX,
        cache(&folder).file_name().unwrap().to_str().unwrap()
    );
    let inode = i64::try_from(std::fs::symlink_metadata(cache(&folder)).unwrap().ino()).unwrap();
    set(
        &engine,
        "INSERT INTO projects_delete_files(name,inode) VALUES (?1,?2)",
        rusqlite::params![stale, inode],
    );
    let removed = owner.remove_project(service, 3).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    while (cache(&service_folder).exists()
        || engine.rows("SELECT count(*) FROM projects_delete_files") > 0)
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !cache(&service_folder).exists(),
        "the removed project's cache goes"
    );
    assert!(
        cache(&folder)
            .join("sandboxed-worktree")
            .join("uv")
            .is_dir(),
        "the cache a project added again uses stays"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects_delete_files"),
        0,
        "the stale record is cleared"
    );
    drop(owner);
    engine.stop().await;
}
