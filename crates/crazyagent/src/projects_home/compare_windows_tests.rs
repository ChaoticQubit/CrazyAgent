use super::{
    AddMode, ProjectId, Registered, check_add, holds_owner_home, move_holds, one_spelling,
    project_move_check, project_move_holds, project_move_lands,
};
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

fn code<T>(result: Result<T, crate::engine::EngineError>) -> String {
    result.err().map(|refusal| refusal.0).unwrap_or_default()
}

#[test]
fn a_folder_holding_an_owner_home_named_with_an_apostrophe_holds_it() {
    let (_temp, base) = plain_temp();
    let home = real(&base);
    for name in ["OBrien", "O'Brien"] {
        let owner = real(&base.join(name));
        assert_eq!(
            owner.to_str().unwrap().starts_with(r"\\?\"),
            name.contains('\''),
            "one_spelling keeps a name the sandbox refuses verbatim"
        );
        assert!(
            holds_owner_home(&home, &owner),
            "the folder above the owner home {name} holds it in either spelling (rv-sec2 #736 L1)"
        );
    }
}

#[test]
fn a_folder_named_with_an_apostrophe_inside_the_projects_home_is_refused() {
    let (_temp, base) = plain_temp();
    let (_engine, engine_root) = plain_temp();
    let home = real(&base.join("home"));
    for name in ["its", "it's"] {
        let source = home.join(name);
        std::fs::create_dir(&source).unwrap();
        assert_eq!(
            code(check_add(
                &home,
                &engine_root,
                source.to_str().unwrap(),
                AddMode::Copy,
                &[],
                &[]
            )),
            "home_source_in_home",
            "a folder {name} already in the projects home is refused (rv-sec2 #736 L1)"
        );
    }
}

#[test]
fn a_move_of_a_folder_named_with_an_apostrophe_inside_a_project_is_refused() {
    let (_temp, base) = plain_temp();
    let (_engine, engine_root) = plain_temp();
    let home = real(&base.join("home"));
    let project = real(&base.join("work").join("a"));
    let registered = [registered("project-a", &project)];
    for name in ["its", "it's"] {
        let source = project.join(name);
        std::fs::create_dir(&source).unwrap();
        assert_eq!(
            code(check_add(
                &home,
                &engine_root,
                source.to_str().unwrap(),
                AddMode::Move,
                &[],
                &registered
            )),
            "home_move_project",
            "a Move of a folder {name} inside a set-up project is refused (rv-sec2 #736 L1)"
        );
    }
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
fn a_home_move_holds_a_folder_named_with_an_apostrophe_inside_its_origin() {
    let (_temp, base) = plain_temp();
    let origin = real(&base.join("home"));
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE projects_home_move (singleton INTEGER, origin TEXT)")
        .unwrap();
    db.execute(
        "INSERT INTO projects_home_move (singleton, origin) VALUES (1, ?1)",
        params![origin.to_str().unwrap()],
    )
    .unwrap();
    for name in ["its", "it's"] {
        let folder = real(&origin.join(name));
        assert!(
            move_holds(&db, &folder).unwrap(),
            "a pending home move holds {name} inside its origin in either spelling (rv-sec2 #736 L1)"
        );
    }
}

#[test]
fn a_project_move_holds_and_lands_a_folder_named_with_an_apostrophe() {
    let (_temp, base) = plain_temp();
    let origin = real(&base.join("work").join("a"));
    let target = real(&base.join("home").join("a"));
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE projects_home_project_move (singleton INTEGER, origin TEXT, target TEXT)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO projects_home_project_move (singleton, origin, target) VALUES (1, ?1, ?2)",
        params![origin.to_str().unwrap(), target.to_str().unwrap()],
    )
    .unwrap();
    for name in ["its", "it's"] {
        assert!(
            project_move_holds(&db, &real(&origin.join(name))).unwrap(),
            "a pending project move holds {name} inside its origin (rv-sec2 #736 L1)"
        );
        assert!(
            project_move_lands(&db, &real(&target.join(name))).unwrap(),
            "a pending project move lands on {name} inside its target (rv-sec2 #736 L1)"
        );
    }
}

#[test]
fn a_project_named_with_an_apostrophe_already_in_the_home_is_not_moved_there() {
    let (_temp, base) = plain_temp();
    let home = real(&base.join("home"));
    for name in ["its", "it's"] {
        let folder = real(&home.join(name));
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
        assert_eq!(
            code(project_move_check(
                &tx,
                &found,
                &found[0].project_id,
                AddMode::Move
            )),
            "move_project_already_home",
            "a project {name} already in the projects home is not moved there (rv-sec2 #736 L1)"
        );
    }
}

#[test]
fn a_path_is_canonical_only_in_the_spelling_one_spelling_stores_and_never_through_a_junction() {
    let (_temp, base) = plain_temp();
    let plain = real(&base.join("work"));
    let verbatim = std::fs::canonicalize(&plain).unwrap();
    let odd = real(&base.join("it's"));
    let link = base.join("link");
    let mut junction = std::process::Command::new("cmd");
    junction
        .args([
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            plain.as_os_str(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let made = crate::spawn_gate::output(&mut junction).unwrap();
    assert!(made.status.success(), "{made:?}");
    assert!(
        super::is_canonical(&plain).unwrap(),
        "a plain folder is canonical (W3-4c)"
    );
    assert!(
        !super::is_canonical(&verbatim).unwrap(),
        "its verbatim twin is not the stored spelling"
    );
    assert!(
        odd.to_str().unwrap().starts_with(r"\\?\") && super::is_canonical(&odd).unwrap(),
        "a folder named with an apostrophe is canonical in the verbatim spelling one_spelling keeps"
    );
    assert!(
        !super::is_canonical(&link).unwrap(),
        "a junction is not canonical"
    );
}
