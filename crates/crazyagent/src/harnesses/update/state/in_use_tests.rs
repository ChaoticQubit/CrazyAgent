use super::*;

/// Adds the team runtime tables and, when asked, a bare agent runs table to a database,
/// with one row per harness name and status. Inputs: the database, the team agents and
/// the agent runs. Output: none; a failed write panics.
fn fill(db: &Connection, agents: &[(&str, &str)], runs: Option<&[(&str, &str)]>) {
    crate::team::initialize(db).unwrap();
    for (harness, status) in agents {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES('p-1','sub_agent',?1,1,?2,1,1)",
            rusqlite::params![harness, status],
        )
        .unwrap();
    }
    let Some(runs) = runs else { return };
    db.execute_batch("CREATE TABLE agent_runs(harness TEXT NOT NULL, state TEXT NOT NULL)")
        .unwrap();
    for (harness, state) in runs {
        db.execute(
            "INSERT INTO agent_runs(harness,state) VALUES(?1,?2)",
            rusqlite::params![harness, state],
        )
        .unwrap();
    }
}

/// An in-memory database filled as [`fill`] does. Inputs: the team agents and the agent
/// runs. Output: the database.
fn live_db(agents: &[(&str, &str)], runs: Option<&[(&str, &str)]>) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    fill(&db, agents, runs);
    db
}

/// A team agent of a harness that is starting, idle or working runs it, whether it is
/// stored under the catalog id or an alias.
#[test]
fn a_live_team_agent_of_a_harness_by_its_id_or_an_alias_is_in_use() {
    for status in ["starting", "idle", "working"] {
        assert_eq!(
            in_use(&live_db(&[("codex", status)], None), "codex"),
            Ok(true)
        );
    }
    let alias = live_db(&[("claude", "working")], None);
    assert_eq!(in_use(&alias, "claude-code"), Ok(true));
}

/// A team agent that ended, or one of another harness, runs nothing the update replaces.
#[test]
fn an_ended_team_agent_or_another_harness_is_not_in_use() {
    let ended = live_db(&[("codex", "stopped"), ("codex", "failed")], None);
    assert_eq!(in_use(&ended, "codex"), Ok(false));
    assert_eq!(
        in_use(&live_db(&[("claude", "working")], None), "codex"),
        Ok(false)
    );
}

/// An agent run still running uses its harness; one that closed does not.
#[test]
fn a_running_agent_run_is_in_use_and_a_closed_one_is_not() {
    assert_eq!(
        in_use(&live_db(&[], Some(&[("codex", "running")])), "codex"),
        Ok(true)
    );
    assert_eq!(
        in_use(&live_db(&[], Some(&[("codex", "closed")])), "codex"),
        Ok(false)
    );
}

/// A database that never made the session tables holds no session, so it refuses
/// nothing; this is the Windows engine, which keeps no agent runs.
#[test]
fn a_database_without_the_session_tables_has_nothing_in_use() {
    assert_eq!(
        in_use(&Connection::open_in_memory().unwrap(), "codex"),
        Ok(false)
    );
}

/// A job whose turn came reads the sessions again from the database file: a live one
/// refuses with the in-use code, none lets it go on, and a database with no file to open
/// again refuses as unavailable.
#[test]
fn a_job_reads_the_sessions_again_from_the_database_file() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("engine.db");
    let path = file.to_str().unwrap();
    fill(
        &Connection::open(&file).unwrap(),
        &[("codex", "working")],
        None,
    );
    assert_eq!(
        reopened(path).and_then(|db| unused(&db, "codex")),
        Err(IN_USE)
    );
    assert_eq!(
        reopened(path).and_then(|db| unused(&db, "claude-code")),
        Ok(())
    );
    assert!(matches!(reopened(""), Err(UNAVAILABLE)));
    let missing = folder.path().join("missing.db");
    assert!(matches!(
        reopened(missing.to_str().unwrap()),
        Err(UNAVAILABLE)
    ));
}

/// A database with the update settings, the team runtime tables and Check and update on,
/// holding one team agent of a harness in a status. Inputs: the stored harness name and
/// the status. Output: the database.
fn sessions_db(harness: &str, status: &str) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let settings = UpdateSettings {
        mode: UpdateMode::CheckAndUpdate,
        ..CLOSED
    };
    set_settings(&db, settings).unwrap();
    fill(&db, &[(harness, status)], None);
    db
}

/// The catalog entry of a harness, found at a file under a fresh folder, and roots with
/// no tool. Input: the catalog id. Output: the folder, which holds the file, the entry and
/// the roots.
fn found_entry(id: &str) -> (tempfile::TempDir, HarnessEntry, Roots) {
    let folder = tempfile::tempdir().unwrap();
    let mut entry = harnesses::catalog(&harnesses::Search::new(Vec::new(), None, Vec::new()))
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap();
    entry.executable = Some(folder.path().join(format!("{id}.exe")));
    let roots = Roots {
        home: folder.path().to_path_buf(),
        brew_prefixes: Vec::new(),
        casks: false,
        npm: None,
        places: crate::tools::tools_new(Vec::new(), None),
    };
    (folder, entry, roots)
}

/// On Windows an update of a harness that a live session runs, named by its catalog id
/// or by an alias, is refused before anything starts, since a running harness's image
/// cannot be replaced; a session that ended refuses nothing.
#[cfg(windows)]
#[test]
fn a_windows_update_of_a_harness_a_live_session_runs_is_refused() {
    for (id, stored, status) in [
        ("codex", "codex", "working"),
        ("codex", "codex", "starting"),
        ("claude-code", "claude", "idle"),
    ] {
        let (_folder, entry, roots) = found_entry(id);
        let refused = Updates::new().start(&sessions_db(stored, status), &entry, &roots, |_| {});
        assert_eq!(refused, Err(IN_USE), "{id} {stored} {status}");
    }
    let (_folder, entry, roots) = found_entry("codex");
    let ended = Updates::new().start(&sessions_db("codex", "stopped"), &entry, &roots, |_| {});
    assert_ne!(ended, Err(IN_USE));
    let other = Updates::new().start(&sessions_db("claude", "working"), &entry, &roots, |_| {});
    assert_ne!(other, Err(IN_USE));
}

/// On macOS and Linux a live session refuses no update, as before: the system replaces
/// the file under a running harness, and refusing there is the owner's call.
#[cfg(not(windows))]
#[test]
fn an_update_outside_windows_is_not_refused_for_a_live_session() {
    let (_folder, entry, roots) = found_entry("codex");
    let started = Updates::new().start(&sessions_db("codex", "working"), &entry, &roots, |_| {});
    assert_ne!(started, Err(IN_USE));
}
