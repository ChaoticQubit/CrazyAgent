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
        plan.run_with(
            std::time::Duration::from_millis(300),
            &record,
            &crate::projects_home::Tally::default()
        ),
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
        plan.run_with(
            std::time::Duration::from_secs(10),
            &stopped_first,
            &crate::projects_home::Tally::default()
        ),
        Ok(())
    );
    stopper.join().unwrap();
    assert_eq!(*moved.borrow(), vec![web.clone(), api.clone()]);

    let first_fails =
        |_: &Path| -> Result<(), EngineError> { Err(EngineError("home_trash_failed".into())) };
    assert_eq!(
        plan.run_with(
            std::time::Duration::from_secs(1),
            &first_fails,
            &crate::projects_home::Tally::default()
        ),
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
        plan.run_with(
            std::time::Duration::from_secs(1),
            &second_fails,
            &crate::projects_home::Tally::default()
        ),
        Err("delete_trash_partial".to_owned())
    );
    assert_eq!(calls.get(), 2);

    std::fs::remove_dir(&api).unwrap();
    std::os::unix::fs::symlink(&base, &api).unwrap();
    moved.borrow_mut().clear();
    assert_eq!(
        plan.run_with(
            std::time::Duration::from_secs(1),
            &record,
            &crate::projects_home::Tally::default()
        ),
        Err("delete_outside_home".to_owned())
    );
    assert!(
        moved.borrow().is_empty(),
        "a folder swapped for a link after the first check refuses before any move"
    );
}

/// A Delete everything's two folders and the plan over them, with agent 4 of the project
/// still working. Input: the temp root. Output: the database and the plan.
fn delete_with_agent(base: &Path) -> (std::path::PathBuf, crate::projects_home::DeletePlan) {
    let home = base.join("home");
    let (web, api) = (home.join("web"), home.join("api"));
    std::fs::create_dir_all(&web).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    let database = base.join("engine.db");
    let connection = Connection::open(&database).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    connection
        .execute_batch(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES ('demo',1);
             INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (4,'demo','team','codex',1,'working',0,0);",
        )
        .unwrap();
    let plan = crate::projects_home::DeletePlan {
        database: database.clone(),
        project: ProjectId::new("demo").unwrap(),
        home,
        folders: vec![web, api],
        agents: vec![4],
    };
    (database, plan)
}

/// DEC-177 step 2: a Stop asked while a Delete everything waits for its agents ends it with
/// `home_add_cancelled` and nothing moved, and the tally showed the agents it waited for.
#[test]
fn a_stop_while_a_delete_waits_for_its_agents_moves_nothing() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let (_database, plan) = delete_with_agent(&base);
    let tally = std::sync::Arc::new(crate::projects_home::Tally::default());
    let moved = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let running = {
        let (tally, moved) = (std::sync::Arc::clone(&tally), std::sync::Arc::clone(&moved));
        std::thread::spawn(move || {
            let record = |_: &Path| -> Result<(), EngineError> {
                moved.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            };
            plan.run_with(std::time::Duration::from_secs(20), &record, &tally)
        })
    };
    let started = std::time::Instant::now();
    while tally.waiting_for() != Some(1) {
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(tally.stop(), "a stop before the first Trash move is taken");
    assert_eq!(
        running.join().unwrap(),
        Err("home_add_cancelled".to_owned())
    );
    assert_eq!(moved.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(tally.trashing(), None);
}

/// DEC-177 step 2: once a Delete everything commits to its first Trash move a Stop is
/// refused, even one asked from inside that move, and every folder moves, each counted.
#[test]
fn a_stop_asked_during_the_first_trash_move_is_refused_and_every_folder_moves() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let (database, plan) = delete_with_agent(&base);
    Connection::open(&database)
        .unwrap()
        .execute("UPDATE team_agents SET status='stopped' WHERE id=4", [])
        .unwrap();
    let tally = crate::projects_home::Tally::default();
    let refused = std::cell::Cell::new(None);
    let moved = std::cell::RefCell::new(Vec::<std::path::PathBuf>::new());
    let record = |folder: &Path| -> Result<(), EngineError> {
        if refused.get().is_none() {
            assert_eq!(tally.trashing(), Some((0, 2)), "committed before the move");
            refused.set(Some(!tally.stop()));
        }
        moved.borrow_mut().push(folder.to_path_buf());
        Ok(())
    };
    assert_eq!(
        plan.run_with(std::time::Duration::from_secs(1), &record, &tally),
        Ok(())
    );
    assert_eq!(refused.get(), Some(true), "the stop was refused");
    assert!(!tally.stopped());
    assert_eq!(moved.borrow().len(), 2);
    assert_eq!(tally.trashing(), Some((2, 2)));
}

/// Delete everything refuses what can change while it waits, in the plan and again after the
/// agent wait before any folder moves: a folder another project uses (`delete_shared_folder`,
/// naming that project), research of the project that is queued, running or waiting for a
/// pool (`delete_research_running`), and a projects home move still pending
/// (`home_move_in_progress`). Finished research holds nothing back. Runs on macOS only, the
/// one platform that offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
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
        plan.run_with(wait, &record, &crate::projects_home::Tally::default()),
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
            plan.run_with(wait, &record, &crate::projects_home::Tally::default()),
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
        plan.run_with(wait, &record, &crate::projects_home::Tally::default()),
        Err("home_move_in_progress".to_owned())
    );
    assert!(
        moved.borrow().is_empty(),
        "no refused delete moves a folder"
    );
    connection
        .execute("DELETE FROM projects_home_move", [])
        .unwrap();
    assert_eq!(
        plan.run_with(wait, &record, &crate::projects_home::Tally::default()),
        Ok(())
    );
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

/// An add holds the projects home from its plan until its record or settle: the home is not
/// set or moved elsewhere while a running row exists. An engine start removes the staging an
/// add cut short left, deletes nothing else, and keeps only the newest end for the owner,
/// until it is forgotten.
#[test]
fn an_add_holds_the_home_until_settled_and_a_start_reads_how_a_cut_add_ended() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let (source, landed) = (base.join("source"), home.join("source"));
    std::fs::create_dir_all(source.join("lib")).unwrap();
    std::fs::write(source.join("lib/a.txt"), "a\n").unwrap();
    let staging = home.join(".crazyagent-adding-00ff00ff00ff00ff");
    std::fs::create_dir_all(staging.join("lib")).unwrap();
    let mut connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    crate::projects_home::save(&connection, &home).unwrap();
    let insert = |connection: &Connection, request: &str| {
        connection
            .execute(
                "INSERT INTO projects_home_add(request, owner, source, destination, mode)
                 VALUES (?1, 'owner', ?2, ?3, 'move')",
                params![request, source.to_str().unwrap(), landed.to_str().unwrap()],
            )
            .unwrap();
    };
    insert(&connection, "add-1");
    let elsewhere = crate::projects_home::HomeAction::SetHome {
        path: base.join("elsewhere").to_str().unwrap().into(),
    };
    let held = {
        let tx = connection.transaction().unwrap();
        projects_home::execute(&tx, &base.join("engine.db"), &elsewhere).unwrap()
    };
    let held = format!("{held:?}");
    assert!(held.contains("Rejected"), "{held}");
    assert!(held.contains("home_add_in_progress"), "{held}");
    let add_1 = RequestId::new("add-1").unwrap();
    assert_eq!(
        projects_home::add_settle(&mut connection, Some((&add_1, false))).unwrap(),
        1
    );
    assert_eq!(projects_home::add_ended(&connection).unwrap(), None);

    insert(&connection, "add-2");
    assert_eq!(projects_home::add_settle(&mut connection, None).unwrap(), 1);
    assert!(!staging.exists(), "the start removes the cut add's staging");
    assert_eq!(
        std::fs::read_to_string(source.join("lib/a.txt")).unwrap(),
        "a\n"
    );
    let ended = projects_home::add_ended(&connection).unwrap().unwrap();
    assert_eq!(ended.request.as_str(), "add-2");
    assert_eq!(ended.ended, "home_add_interrupted");

    std::fs::create_dir_all(landed.join("lib")).unwrap();
    insert(&connection, "add-3");
    assert_eq!(projects_home::add_settle(&mut connection, None).unwrap(), 1);
    let ended = projects_home::add_ended(&connection).unwrap().unwrap();
    assert_eq!(ended.request.as_str(), "add-3");
    assert_eq!(ended.ended, "home_move_source_left");
    assert_eq!(
        std::fs::read_to_string(source.join("lib/a.txt")).unwrap(),
        "a\n",
        "a start deletes nothing of the old folder"
    );
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM projects_home_add", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "only the newest end is kept");
    let add_3 = RequestId::new("add-3").unwrap();
    projects_home::job_forget(&connection, &add_3).unwrap();
    assert_eq!(projects_home::add_ended(&connection).unwrap(), None);
}

/// RV-DS241-198: an add whose folder landed in the projects home but whose record never
/// came, as when its grant was found stale after the disk changed, is not forgotten by its
/// exchange's settle: its running row ends with what its disk shows, as after an engine stop,
/// so the owner reads where the folder went and nothing is deleted. An add that landed
/// nothing is forgotten as before, and so is one that placed nothing because another folder
/// took its destination after the plan (rv-sec2 b9 F1), while a settle after a record leaves
/// the kept end alone.
#[test]
fn an_add_whose_folder_landed_before_a_record_that_never_came_is_kept_as_ended() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let (source, landed) = (base.join("source"), home.join("source"));
    std::fs::create_dir_all(source.join("lib")).unwrap();
    std::fs::write(source.join("lib/a.txt"), "a\n").unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mut connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    crate::projects_home::save(&connection, &home).unwrap();
    let insert = |connection: &Connection, request: &str, mode: &str| {
        connection
            .execute(
                "INSERT INTO projects_home_add(request, owner, source, destination, mode)
                 VALUES (?1, 'owner', ?2, ?3, ?4)",
                params![
                    request,
                    source.to_str().unwrap(),
                    landed.to_str().unwrap(),
                    mode
                ],
            )
            .unwrap();
    };
    let settle = |connection: &mut Connection, request: &str, placed: bool| {
        let request = RequestId::new(request).unwrap();
        projects_home::add_settle(connection, Some((&request, placed))).unwrap()
    };

    insert(&connection, "add-unlanded", "move");
    assert_eq!(settle(&mut connection, "add-unlanded", true), 1);
    assert_eq!(
        projects_home::add_ended(&connection).unwrap(),
        None,
        "an add that landed nothing is forgotten"
    );

    std::fs::create_dir_all(landed.join("lib")).unwrap();
    insert(&connection, "add-copied", "copy");
    assert_eq!(settle(&mut connection, "add-copied", true), 1);
    let ended = projects_home::add_ended(&connection)
        .unwrap()
        .expect("a landed add whose record never came was forgotten");
    assert_eq!(
        (ended.request.as_str(), ended.mode, ended.ended.as_str()),
        ("add-copied", crate::projects_home::AddMode::Copy, "done")
    );

    insert(&connection, "add-moved", "move");
    assert_eq!(settle(&mut connection, "add-moved", true), 1);
    let ended = projects_home::add_ended(&connection).unwrap().unwrap();
    assert_eq!(
        (ended.request.as_str(), ended.mode, ended.ended.as_str()),
        (
            "add-moved",
            crate::projects_home::AddMode::Move,
            "home_move_source_left"
        )
    );
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM projects_home_add", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "only the newest end is kept");
    assert_eq!(
        std::fs::read_to_string(source.join("lib/a.txt")).unwrap(),
        "a\n",
        "a settle deletes nothing of the old folder"
    );
    assert!(landed.join("lib").is_dir(), "nor of the landed folder");

    assert_eq!(
        settle(&mut connection, "add-moved", true),
        0,
        "a recorded or settled add has no running row"
    );
    let kept = projects_home::add_ended(&connection).unwrap().unwrap();
    assert_eq!(kept.request.as_str(), "add-moved");

    insert(&connection, "add-taken", "copy");
    assert_eq!(settle(&mut connection, "add-taken", false), 1);
    let kept = projects_home::add_ended(&connection).unwrap().unwrap();
    assert_eq!(
        kept.request.as_str(),
        "add-moved",
        "an add that placed nothing is not told as landed in a folder another took"
    );
    let running: i64 = connection
        .query_row(
            "SELECT count(*) FROM projects_home_add WHERE ended IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(running, 0, "the add that placed nothing is forgotten");
    assert!(landed.join("lib").is_dir(), "the other folder stays");
}

/// A delete's mark outlives a settle that only shares its exchange number, since a new engine
/// process numbers exchanges again: a start ends the mark of a project that went and turns
/// the mark of a project still there into a notice that names no exchange, and a later settle
/// ends only the live mark of its own number, failed when the project's records stayed.
#[test]
fn a_delete_mark_shown_to_the_owner_outlives_a_settle_of_a_reused_exchange() {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    crate::ledger::initialize(&connection).unwrap();
    let mark = |project: &str, exchange: i64| {
        connection
            .execute(
                "INSERT INTO ledger_actions(kind,action_key,payload,started_ms)
                 VALUES ('project_delete',?1,json_object('project',?1,'exchange',?2),1)",
                params![project, exchange],
            )
            .unwrap();
    };
    let marks = || -> Vec<(String, String)> {
        let mut statement = connection
            .prepare(
                "SELECT action_key,payload FROM ledger_actions
                 WHERE kind='project_delete' AND outcome IS NULL ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let outcome = |project: &str| -> Option<String> {
        connection
            .query_row(
                "SELECT outcome FROM ledger_actions WHERE action_key=?1 ORDER BY id DESC",
                [project],
                |row| row.get(0),
            )
            .unwrap()
    };
    connection
        .execute_batch("INSERT INTO projects(id, body) VALUES ('kept','{}'),('next','{}')")
        .unwrap();
    mark("kept", 7);
    mark("gone", 8);
    projects_home::delete_settle(&connection, None, false).unwrap();
    let shown = marks();
    assert_eq!(
        shown.len(),
        1,
        "a start ends the mark of a project that went: {shown:?}",
    );
    assert_eq!(shown[0].0, "kept");
    assert_eq!(outcome("gone").as_deref(), Some("done"));
    let payload: serde_json::Value = serde_json::from_str(&shown[0].1).unwrap();
    assert_eq!(payload["unknown"], true, "the kept project's mark is shown");
    assert!(
        payload.get("exchange").is_none(),
        "a shown mark names no exchange",
    );
    mark("next", 7);
    projects_home::delete_settle(&connection, Some(7), false).unwrap();
    let left = marks();
    assert_eq!(
        left.len(),
        1,
        "a settle ends only the live mark of its number: {left:?}",
    );
    assert_eq!(left[0].0, "kept");
    assert_eq!(
        outcome("next").as_deref(),
        Some("failed"),
        "its records stayed",
    );
}

/// DEC-177 add.1: a delete's mark carries its job's request, the project's name and how many
/// folders it moves, and keeps them when a settle after a Trash move or an engine start shows
/// it as cut short, so the start can show the delete as the engine's job. A mark from 2.4.1
/// that names no request shows on the banner only.
#[test]
fn a_delete_mark_keeps_its_request_name_and_count_when_it_shows_cut_short() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    connection
        .execute_batch("INSERT INTO projects(id, body) VALUES ('meals','{}'),('older','{}')")
        .unwrap();
    let meals = ProjectId::new("meals").unwrap();
    let request = crate::protocol::RequestId::new("delete-7").unwrap();
    let mark = crate::projects_home::delete_mark::begin(
        &connection,
        &meals,
        7,
        &request,
        "Meal planner",
        2,
    )
    .unwrap();
    assert_eq!(mark, Ok(()));
    assert_eq!(
        crate::projects_home::delete_mark::cut_short(&connection).unwrap(),
        None,
        "a live delete is not cut short"
    );
    assert_eq!(
        crate::projects_home::delete_mark::settle(&connection, 7, true).unwrap(),
        1
    );
    let shown = crate::projects_home::delete_mark::CutShort {
        project: meals.clone(),
        request: request.clone(),
        name: "Meal planner".into(),
        total: 2,
    };
    assert_eq!(
        crate::projects_home::delete_mark::cut_short(&connection).unwrap(),
        Some(shown.clone())
    );
    assert_eq!(
        crate::projects_home::delete_mark::start(&connection).unwrap(),
        1
    );
    assert_eq!(
        crate::projects_home::delete_mark::cut_short(&connection).unwrap(),
        Some(shown),
        "an engine start keeps what the job needs"
    );
    assert!(crate::projects_home::delete_mark::forget(&connection, &meals).unwrap());
    let _older = crate::ledger::begin(
        &connection,
        crate::ledger::ActionKind::ProjectDelete,
        "older",
        r#"{"project":"older","exchange":8}"#,
    )
    .unwrap();
    assert_eq!(
        crate::projects_home::delete_mark::start(&connection).unwrap(),
        1
    );
    assert_eq!(
        crate::projects_home::delete_mark::cut_short(&connection).unwrap(),
        None,
        "a 2.4.1 mark names no job"
    );
}

/// RV-DS241-152: a delete records every engine file name the engine makes, up to the
/// `RECORDED_NAME_BYTES_MAX` bytes of an Overview or roadmap status run of a long project
/// id, and a name the record cannot hold fails the write instead of going unrecorded, so no
/// run folder of a deleted project is left unfound. A name recorded twice stays one record.
/// The name is built from that constant, so a wider one fails here until the record's
/// check follows it (G1 code r1 F1).
#[test]
fn a_delete_records_run_names_up_to_the_recorded_width_and_refuses_a_longer_one() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let mut connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    let width = crate::projects_home::RECORDED_NAME_BYTES_MAX;
    let pad = width - "overview--1794000000000".len();
    let name = format!("overview-{}-1794000000000", "a".repeat(pad));
    assert_eq!(name.len(), width);
    let count = |connection: &Connection, name: &str| -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM projects_delete_files WHERE name=?1",
                [name],
                |row| row.get(0),
            )
            .unwrap()
    };

    let twice = [(name.clone(), 7), (name.clone(), 7)];
    let tx = connection.transaction().unwrap();
    projects_home::record_files(&tx, &twice).unwrap();
    tx.commit().unwrap();
    let longer = format!("{name}0");
    let tx = connection.transaction().unwrap();
    let refused = projects_home::record_files(&tx, &[(longer.clone(), 8)]);
    drop(tx);

    assert_eq!(
        count(&connection, &name),
        1,
        "a run name of the recorded width is recorded once"
    );
    assert!(
        refused.is_err(),
        "a name the record cannot hold fails the write"
    );
    assert_eq!(count(&connection, &longer), 0);
}

/// RV-DS241-149: an engine start forgets the repository admissions of projects that are no
/// longer recorded, which removals before the fix left behind, so a project added again
/// under such an id admits its repository afresh; a recorded project keeps its admission.
#[test]
fn an_engine_start_forgets_the_admissions_of_projects_that_are_gone() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    let kept = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: ProjectId::new("kept").unwrap(),
        spec: crate::protocol::ProjectSpec {
            name: "Kept".into(),
            repositories: vec![],
        },
    })
    .unwrap();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('kept', ?1)", [kept])
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO repository_admissions(project,repository,body) VALUES
               ('kept','/repos/kept','{}'), ('gone','/repos/gone','{}');",
        )
        .unwrap();

    initialize_authority_schemas(&connection).unwrap();

    let left: Vec<String> = connection
        .prepare("SELECT project FROM repository_admissions ORDER BY project")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(left, ["kept"]);
}

/// DEC-177 add.2 Q3 A: a New Project's ledger mark names the job's request and the project's
/// name; a second build of the same project ends the first mark, the record ends the mark done,
/// and an engine start ends every mark left: done when its project was recorded, else failed,
/// returning the newest failed one so the start shows it as the job cut short. A second start
/// finds none.
#[test]
fn a_setup_mark_ends_with_its_record_and_a_start_ends_the_rest_naming_the_newest_cut_short() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let connection = Connection::open(base.join("engine.db")).unwrap();
    initialize_authority_schemas(&connection).unwrap();
    connection
        .execute_batch("INSERT INTO projects(id, body) VALUES ('kept','{}')")
        .unwrap();
    let mark = |project: &str, request: &str, name: &str| {
        crate::projects_home::setup_mark::begin(
            &connection,
            &ProjectId::new(project).unwrap(),
            &crate::protocol::RequestId::new(request).unwrap(),
            name,
        )
        .unwrap()
    };
    let open = |key: &str| -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM ledger_actions
                 WHERE kind='project_setup' AND action_key=?1 AND outcome IS NULL",
                [key],
                |row| row.get(0),
            )
            .unwrap()
    };
    let ended = |key: &str| -> Vec<String> {
        let mut statement = connection
            .prepare(
                "SELECT outcome FROM ledger_actions
                 WHERE kind='project_setup' AND action_key=?1 AND outcome IS NOT NULL ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([key], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    mark("meals", "setup-1", "Meal planner");
    mark("meals", "setup-2", "Meal planner");
    assert_eq!(open("meals"), 1, "a second build ends the first mark");
    assert_eq!(ended("meals"), ["failed"]);
    let meals = ProjectId::new("meals").unwrap();
    assert!(crate::projects_home::setup_mark::end(&connection, &meals, true).unwrap());
    assert_eq!(open("meals"), 0);
    assert_eq!(ended("meals"), ["failed", "done"]);
    assert!(!crate::projects_home::setup_mark::end(&connection, &meals, false).unwrap());

    mark("older", "setup-3", "Older");
    mark("newer", "setup-4", "Newer");
    mark("kept", "setup-5", "Kept");
    let cut = crate::projects_home::setup_mark::start(&connection)
        .unwrap()
        .expect("the newest New Project an engine stop cut short");
    assert_eq!(cut.project, ProjectId::new("newer").unwrap());
    assert_eq!(cut.request.as_str(), "setup-4");
    assert_eq!(cut.name, "Newer");
    for key in ["older", "newer", "kept"] {
        assert_eq!(open(key), 0, "{key}");
    }
    assert_eq!(ended("older"), ["failed"]);
    assert_eq!(ended("newer"), ["failed"]);
    assert_eq!(
        ended("kept"),
        ["done"],
        "a recorded project's mark ends done"
    );
    assert_eq!(
        crate::projects_home::setup_mark::start(&connection).unwrap(),
        None
    );
}
