use super::{busy, checked, landing_repositories, names_inside};
use crate::projects_home::{AddMode, Registered};
use crate::protocol::ProjectId;
use kernel_sandbox::paths::one_spelling;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, base)
}

fn real(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    one_spelling(std::fs::canonicalize(path).unwrap())
}

fn registered(id: &str, folder: &Path) -> Registered {
    Registered {
        project_id: ProjectId::new(id).unwrap(),
        name: id.into(),
        folder: folder.to_path_buf(),
        repositories: Vec::new(),
        onboarding: false,
    }
}

#[test]
fn a_project_stored_in_its_plain_spelling_is_not_refused_as_a_link() {
    let (_temp, base) = plain_temp();
    let home = real(&base.join("home"));
    let folder = real(&base.join("work").join("a"));
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE projects_home (singleton INTEGER, path TEXT);
         CREATE TABLE project_setups (project TEXT)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO projects_home (singleton, path) VALUES (1, ?1)",
        params![home.to_str().unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO project_setups (project) VALUES ('project-a')",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let found = [registered("project-a", &folder)];
    let checked = checked(&tx, &found, &found[0].project_id, AddMode::Move)
        .map_err(|refusal| refusal.0)
        .unwrap();
    assert_eq!(
        checked.real, folder,
        "the move carries the project's stored plain spelling (W3-4c)"
    );
}

#[test]
fn a_move_destination_names_a_project_inside_it_named_with_an_apostrophe() {
    let (_temp, base) = plain_temp();
    let destination = real(&base.join("home").join("b"));
    for name in ["its", "it's"] {
        let used = real(&destination.join(name));
        assert!(
            names_inside(&destination, &registered("project-b", &used)),
            "a project folder {name} inside the destination is named there (rv-sec2 #739 R2-L1)"
        );
    }
}

#[test]
fn a_landing_repository_named_with_an_apostrophe_under_the_origin_is_collected() {
    let (_temp, base) = plain_temp();
    let origin = real(&base.join("work").join("a"));
    let project = ProjectId::new("project-a").unwrap();
    for name in ["its", "it's"] {
        let repository = real(&origin.join(name));
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE landing_change_sets (project TEXT, repository TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO landing_change_sets (project, repository) VALUES (?1, ?2)",
            params![project.as_str(), repository.to_str().unwrap()],
        )
        .unwrap();
        let roots = landing_repositories(&db, &project, &origin).unwrap();
        assert!(
            roots.contains(&repository),
            "a landing repository {name} under the origin is collected for the move (W3-4c)"
        );
    }
}

/// CR-1706 (W3-4c r2): a Reserved or Ready lease on a repository nested under the project's
/// folder makes the move wait on Windows, where stored paths part with `\`, as one on the
/// folder itself does; a lease on a sibling folder that only shares the name's start does
/// not.
#[test]
fn a_reserved_lease_on_a_nested_repository_blocks_a_windows_move() {
    let (_temp, base) = plain_temp();
    let origin = real(&base.join("work").join("a"));
    let nested = real(&origin.join("repo"));
    let sibling = real(&base.join("work").join("ab").join("repo"));
    let project = ProjectId::new("project-a").unwrap();
    let origin_text = origin.to_str().unwrap();
    for (repository, state, blocks) in [
        (&origin, "Reserved", true),
        (&nested, "Reserved", true),
        (&nested, "Ready", true),
        (&nested, "Released", false),
        (&sibling, "Reserved", false),
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE worktree_leases (task TEXT, repository TEXT, path TEXT, body TEXT)",
        )
        .unwrap();
        db.execute(
            "INSERT INTO worktree_leases (task, repository, path, body) VALUES ('task-1', ?1, ?2, ?3)",
            params![
                repository.to_str().unwrap(),
                base.join("engine").join("lease-1").to_str().unwrap(),
                format!(r#"{{"lease":{{"state":"{state}"}}}}"#)
            ],
        )
        .unwrap();
        let found = busy(&db, &project, origin_text)
            .map_err(|refusal| refusal.0)
            .unwrap();
        assert_eq!(
            found,
            blocks.then_some("move_project_master_session"),
            "a {state} lease on {} with the project at {origin_text}",
            repository.display()
        );
    }
}
