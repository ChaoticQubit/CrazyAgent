use super::engine_root;
use std::path::PathBuf;

#[test]
fn the_engine_root_is_named_plain_so_a_projects_home_holding_it_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let data = base.join("data");
    std::fs::create_dir(&data).unwrap();
    let root = engine_root(&data.join("engine.db")).unwrap();
    assert_eq!(
        root, data,
        "the engine root is in the one plain spelling, as one_spelling gives every folder it is compared with (rv-sec2 #717 C1)"
    );
    assert_eq!(
        crate::projects_home::check_home(&base, &root)
            .unwrap_err()
            .0,
        "projects_home_unsafe",
        "a projects home that holds the engine root is refused"
    );
}

#[test]
fn agent_work_named_with_an_apostrophe_inside_the_projects_home_is_held_there() {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let home = base.join("home");
    for name in ["its", "it's"] {
        let worktree = home.join(name).join("agent-1");
        std::fs::create_dir_all(&worktree).unwrap();
        let worktree =
            kernel_sandbox::paths::one_spelling(std::fs::canonicalize(&worktree).unwrap());
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE team_worktrees (repository TEXT, worktree TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO team_worktrees (repository, worktree) VALUES (?1, ?1)",
            rusqlite::params![worktree.to_str().unwrap()],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(
            super::held_inside(
                &tx,
                &home,
                "team_worktrees",
                "SELECT repository, worktree FROM team_worktrees",
                rusqlite::params![],
            )
            .unwrap(),
            "an agent worktree under {name} in the projects home holds a home move (rv-sec2 #736 L1)"
        );
    }
}
