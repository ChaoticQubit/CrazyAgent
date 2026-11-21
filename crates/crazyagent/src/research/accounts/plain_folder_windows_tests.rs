use super::SettingsResolver;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

fn plain(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    let full = std::fs::canonicalize(path).unwrap();
    PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap())
}

fn setup(id: &str, folder: &Path) -> String {
    let setup = crate::project_setup::StoredSetup {
        project_id: crate::protocol::ProjectId::new(id).unwrap(),
        name: id.into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    };
    serde_json::to_string(&setup).unwrap()
}

#[test]
fn a_project_folder_in_its_plain_spelling_is_resolved_stored_or_under_the_projects_folder() {
    let temp = tempfile::tempdir().unwrap();
    let base = plain(temp.path());
    let projects = plain(&base.join("projects"));
    let stored = plain(&base.join("elsewhere").join("project-a"));
    let fallback = plain(&projects.join("project-b"));
    let database = base.join("engine.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE project_setups (project TEXT, body TEXT)")
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups (project, body) VALUES ('project-a', ?1)",
            params![setup("project-a", &stored)],
        )
        .unwrap();
    let resolver = SettingsResolver::new(
        &base.join("global.json"),
        &base.join("project.json"),
        &projects,
        &database,
    )
    .unwrap();
    assert_eq!(
        resolver.plain_folder(Some(&connection), "project-a"),
        Some(stored),
        "a folder the engine stored plain resolves (W3-4c)"
    );
    assert_eq!(
        resolver.plain_folder(Some(&connection), "project-b"),
        Some(fallback),
        "a folder under the projects folder resolves in the same plain spelling (W3-4c)"
    );
}
