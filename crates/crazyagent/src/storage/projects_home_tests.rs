use super::*;

/// The last check before a home move's first write sees agent work that appeared under the
/// move's hold, so the move is refused before any write; with none it lets the move go,
/// and an exchange with no pending move is an error.
#[test]
fn a_home_move_rechecks_agent_work_under_its_hold_before_the_first_write() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    std::fs::create_dir(&home).unwrap();
    let mut connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    crate::projects_home::save(&connection, &home).unwrap();
    connection
        .execute(
            "INSERT INTO projects_home_move VALUES (1,'request-7',7,?1,?2,'[\"demo\"]')",
            params![home.to_str().unwrap(), base.join("next").to_str().unwrap()],
        )
        .unwrap();
    assert_eq!(projects_home::recheck(&mut connection, 7).unwrap(), Ok(()));
    let repository = home.join("demo");
    connection
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (3,'demo',?1,?2,?3)",
            params![
                repository.to_str().unwrap(),
                repository.join("agent-3").to_str().unwrap(),
                repository.join("agent-3").join(".git").to_str().unwrap()
            ],
        )
        .unwrap();
    assert_eq!(
        projects_home::recheck(&mut connection, 7).unwrap(),
        Err("home_move_agent_work".to_owned())
    );
    assert_eq!(
        projects_home::recheck(&mut connection, 8).unwrap_err().0,
        "home_move_not_pending"
    );
}

/// A Delete everything moves nothing while an agent of the project runs: past the wait it
/// refuses `delete_agents_running`, and once the agent stops every move sees the Stop all row
/// and no running agent of the project. A first failed move refuses `home_trash_failed`, a
/// later one `delete_trash_partial`, and a folder swapped for a link refuses before any move.
#[test]
fn delete_everything_moves_folders_only_after_the_projects_agents_stop() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let web = home.join("web");
    let api = home.join("api");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    let database = base.join("engine.db");
    let connection = Connection::open(&database).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    connection
        .execute_batch(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES ('demo',1);
             INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (4,'demo','team','codex',1,'working',0,0),
                    (5,'other','team','codex',1,'working',0,0);",
        )
        .unwrap();
    let plan = crate::projects_home::DeletePlan {
        database: database.clone(),
        project: ProjectId::new("demo").unwrap(),
        home: home.clone(),
        folders: vec![web.clone(), api.clone()],
        agents: vec![4],
    };
    let moved = std::cell::RefCell::new(Vec::<PathBuf>::new());
    let record = |folder: &Path| -> Result<(), EngineError> {
        moved.borrow_mut().push(folder.to_path_buf());
        Ok(())
    };
    assert_eq!(
        plan.run_with(std::time::Duration::from_millis(300), &record),
        Err("delete_agents_running".to_owned())
    );
    assert!(
        moved.borrow().is_empty(),
        "nothing moves while an agent of the project runs"
    );

    let stopper = {
        let database = database.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            Connection::open(&database)
                .unwrap()
                .execute("UPDATE team_agents SET status='stopped' WHERE id=4", [])
                .unwrap();
        })
    };
    let stopped_first = |folder: &Path| -> Result<(), EngineError> {
        let reader =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let running: i64 = reader
            .query_row(
                "SELECT COUNT(*) FROM team_agents WHERE project='demo' AND status NOT IN ('stopped','failed')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let paused: i64 = reader
            .query_row(
                "SELECT COUNT(*) FROM team_paused_projects WHERE project='demo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            (running, paused),
            (0, 1),
            "a folder moves only once Stop all holds and the project's agents stopped"
        );
        moved.borrow_mut().push(folder.to_path_buf());
        Ok(())
    };
    assert_eq!(
        plan.run_with(std::time::Duration::from_secs(10), &stopped_first),
        Ok(())
    );
    stopper.join().unwrap();
    assert_eq!(*moved.borrow(), vec![web.clone(), api.clone()]);

    let first_fails = |_: &Path| -> Result<(), EngineError> {
        Err(EngineError("home_trash_failed".into()))
    };
    assert_eq!(
        plan.run_with(std::time::Duration::from_secs(1), &first_fails),
        Err("home_trash_failed".to_owned())
    );
    let calls = std::cell::Cell::new(0);
    let second_fails = |_: &Path| -> Result<(), EngineError> {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            Ok(())
        } else {
            Err(EngineError("home_trash_failed".into()))
        }
    };
    assert_eq!(
        plan.run_with(std::time::Duration::from_secs(1), &second_fails),
        Err("delete_trash_partial".to_owned())
    );
    assert_eq!(calls.get(), 2);

    std::fs::remove_dir(&api).unwrap();
    std::os::unix::fs::symlink(&base, &api).unwrap();
    moved.borrow_mut().clear();
    assert_eq!(
        plan.run_with(std::time::Duration::from_secs(1), &record),
        Err("delete_outside_home".to_owned())
    );
    assert!(
        moved.borrow().is_empty(),
        "a folder swapped for a link after the first check refuses before any move"
    );
}
