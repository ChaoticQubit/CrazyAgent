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
    let moved = std::cell::RefCell::new(Vec::<std::path::PathBuf>::new());
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

    let first_fails =
        |_: &Path| -> Result<(), EngineError> { Err(EngineError("home_trash_failed".into())) };
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

/// Delete everything refuses what can change while it waits, in the plan and again after the
/// agent wait before any folder moves: a folder another project uses (`delete_shared_folder`,
/// naming that project), research of the project that is queued, running or waiting for a
/// pool (`delete_research_running`), and a projects home move still pending
/// (`home_move_in_progress`). Finished research holds nothing back.
#[test]
fn delete_everything_refuses_a_shared_folder_open_research_and_a_pending_move_before_any_move() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let web = home.join("web");
    std::fs::create_dir_all(web.join("lib")).unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    crate::projects_home::save(&connection, &home).unwrap();
    let declare = |connection: &Connection, id: &str, name: &str, repository: &Path| {
        let body = serde_json::json!({
            "project_id": id,
            "spec": { "name": name, "repositories": [repository.to_str().unwrap()] },
        });
        connection
            .execute(
                "INSERT INTO projects(id, body) VALUES (?1, ?2)",
                params![id, body.to_string()],
            )
            .unwrap();
    };
    declare(&connection, "demo", "Demo", &web);
    let project = ProjectId::new("demo").unwrap();
    let check = |connection: &mut Connection| {
        let tx = connection.transaction().unwrap();
        projects_home::delete_check(&tx, &project)
            .unwrap()
            .unwrap()
            .planned
            .map(|(_, moved)| moved)
    };
    assert_eq!(check(&mut connection), Ok(vec![web.clone()]));
    let plan = crate::projects_home::DeletePlan {
        database: database.clone(),
        project: project.clone(),
        home: home.clone(),
        folders: vec![web.clone()],
        agents: vec![],
    };
    let moved = std::cell::RefCell::new(Vec::<std::path::PathBuf>::new());
    let record = |folder: &Path| -> Result<(), EngineError> {
        moved.borrow_mut().push(folder.to_path_buf());
        Ok(())
    };
    let wait = std::time::Duration::from_millis(100);

    declare(&connection, "lib", "Shared lib", &web.join("lib"));
    let refusal = check(&mut connection).unwrap_err();
    assert_eq!(refusal.code, "delete_shared_folder");
    assert!(
        refusal.reason.contains("Shared lib"),
        "the refusal names the other project: {}",
        refusal.reason
    );
    assert_eq!(
        plan.run_with(wait, &record),
        Err("delete_shared_folder".to_owned()),
        "a project registered during the wait refuses before any move"
    );
    connection
        .execute("DELETE FROM projects WHERE id='lib'", [])
        .unwrap();

    for status in ["queued", "running", "needs_pool"] {
        connection
            .execute_batch(&format!(
                "DELETE FROM research_jobs;
                 INSERT INTO research_jobs(id,project,kind,harness,tools,status,created_ms)
                 VALUES (41,'demo','market_research','codex',1,'{status}',0);"
            ))
            .unwrap();
        let refusal = check(&mut connection).unwrap_err();
        assert_eq!(refusal.code, "delete_research_running", "{status}");
        assert!(refusal.reason.contains("research"), "{}", refusal.reason);
        assert_eq!(
            plan.run_with(wait, &record),
            Err("delete_research_running".to_owned()),
            "{status}"
        );
    }
    connection
        .execute("UPDATE research_jobs SET status='done'", [])
        .unwrap();
    assert_eq!(
        check(&mut connection),
        Ok(vec![web.clone()]),
        "finished research holds no delete"
    );

    connection
        .execute(
            "INSERT INTO projects_home_move VALUES (1,'request-7',7,?1,?2,'[\"web\"]')",
            params![home.to_str().unwrap(), base.join("next").to_str().unwrap()],
        )
        .unwrap();
    assert_eq!(
        check(&mut connection).unwrap_err().code,
        "home_move_in_progress"
    );
    assert_eq!(
        plan.run_with(wait, &record),
        Err("home_move_in_progress".to_owned())
    );
    assert!(
        moved.borrow().is_empty(),
        "no refused delete moves a folder"
    );
    connection
        .execute("DELETE FROM projects_home_move", [])
        .unwrap();
    assert_eq!(plan.run_with(wait, &record), Ok(()));
    assert_eq!(*moved.borrow(), vec![web.clone()]);
}

/// T8 R4: Delete everything records the project's tool cache folder as an engine file and
/// removes it, unlinking the engine's `.git` links to `/dev/null` in its buckets and leaving
/// `/dev/null` itself as it was; a file an agent pinned in the cache does not keep it
/// (T8 r1 L3).
#[cfg(target_os = "macos")]
#[test]
fn delete_everything_removes_the_projects_tool_cache_and_leaves_dev_null_alone() {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let web = base.join("home").join("web");
    std::fs::create_dir_all(&web).unwrap();
    let cache = crate::sandbox::tool_cache::project_folder(&base, web.to_str().unwrap());
    let bucket = cache
        .join("sandboxed-worktree")
        .join("uv")
        .join("sdists-v10");
    std::fs::create_dir_all(&bucket).unwrap();
    std::os::unix::fs::symlink("/dev/null", bucket.join(".git")).unwrap();
    std::fs::write(bucket.join("pinned"), "kept").unwrap();
    nix::unistd::chflags(
        bucket.join("pinned").as_path(),
        nix::sys::stat::FileFlag::UF_IMMUTABLE,
    )
    .unwrap();
    std::fs::write(cache.join("sandboxed-worktree.primed"), "record").unwrap();
    let plan = crate::projects_home::DeletePlan {
        database: base.join("engine.db"),
        project: ProjectId::new("demo").unwrap(),
        home: base.join("home"),
        folders: vec![web],
        agents: vec![],
    };
    let files = plan.engine_files(&[]);
    let name = format!(
        "tool-cache-{}",
        cache.file_name().unwrap().to_str().unwrap()
    );
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, [name.as_str()]);
    let null = std::fs::symlink_metadata("/dev/null").unwrap();
    assert_eq!(
        crate::projects_home::remove_engine_files(&plan.database, &files),
        [name]
    );
    assert!(std::fs::symlink_metadata(&cache).is_err());
    assert!(base.join("tool-cache").is_dir());
    let after = std::fs::symlink_metadata("/dev/null").unwrap();
    assert!(after.file_type().is_char_device());
    assert_eq!((after.ino(), after.mode()), (null.ino(), null.mode()));
}

/// Delete everything is offered only on macOS in V2, where the move to the Trash is proven;
/// Windows and Linux offer Remove from dashboard only, and this build answers for its own
/// platform.
#[test]
fn delete_everything_is_offered_only_on_macos_in_v2() {
    assert!(crate::projects_home::delete_qualified_on("macos"));
    for os in ["windows", "linux", "freebsd", ""] {
        assert!(!crate::projects_home::delete_qualified_on(os), "{os:?}");
    }
    assert_eq!(
        crate::projects_home::delete_offered(),
        cfg!(target_os = "macos")
    );
}
