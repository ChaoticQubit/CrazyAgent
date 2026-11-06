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
