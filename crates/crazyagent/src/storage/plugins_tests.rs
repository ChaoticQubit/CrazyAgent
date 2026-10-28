use super::*;
use std::os::unix::fs::PermissionsExt;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    super::super::initialize_schema(&connection).unwrap();
    initialize(&connection).unwrap();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('app', '{}')", [])
        .unwrap();
    (root, connection)
}

fn server(connection: &mut Connection, root: &Path, name: &str, spec: &ServerSpec) -> String {
    let ResultValue::Server(view) = execute(
        connection,
        root,
        Command::AddServer {
            scope: Scope::Global,
            name: name.into(),
            spec: spec.clone(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    view.id
}

fn attach(connection: &mut Connection, root: &Path, id: &str, name: &str, value: &[u8]) {
    let reference = new_secret_reference().unwrap();
    store_secret(root, &reference, value.to_vec()).unwrap();
    execute(
        connection,
        root,
        Command::AttachSecret {
            id: id.into(),
            name: name.into(),
            reference,
        },
    )
    .unwrap();
}

#[test]
fn launch_plugins_carry_secret_values_only_for_approved_servers() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let project = ProjectId::new("app").unwrap();
    let approved = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec!["--stdio".into()],
    };
    let waiting = ServerSpec::Http {
        url: "https://mcp.example.invalid/mcp".into(),
    };
    let first = server(&mut connection, root, "tools", &approved);
    let second = server(&mut connection, root, "remote", &waiting);
    attach(
        &mut connection,
        root,
        &first,
        "API_TOKEN",
        b"dummy-launch-value",
    );
    attach(
        &mut connection,
        root,
        &second,
        "Authorization",
        b"Bearer dummy",
    );
    execute(
        &mut connection,
        root,
        Command::Approve {
            id: first.clone(),
            spec: approved.clone(),
        },
    )
    .unwrap();

    let ResultValue::Launch(launch) = execute(
        &mut connection,
        root,
        Command::Launch {
            project: project.clone(),
        },
    )
    .unwrap() else {
        panic!("launch set required");
    };
    assert!(launch.skills.is_empty());
    assert_eq!(launch.servers.len(), 1);
    let tools = &launch.servers[0];
    assert_eq!(tools.name, "tools");
    assert_eq!(tools.spec, approved);
    assert_eq!(tools.secrets.len(), 1);
    assert_eq!(tools.secrets[0].name, "API_TOKEN");
    assert_eq!(tools.secrets[0].value.expose(), b"dummy-launch-value");
    let shown = format!("{launch:?}");
    assert!(!shown.contains("dummy-launch-value"), "{shown}");
    assert!(shown.contains("API_TOKEN"));

    let ResultValue::Secret(_, Some(reference)) = execute(
        &mut connection,
        root,
        Command::DetachSecret {
            id: first.clone(),
            name: "API_TOKEN".into(),
        },
    )
    .unwrap() else {
        panic!("detached reference required");
    };
    let mut kept = execute(
        &mut connection,
        root,
        Command::AttachSecret {
            id: first,
            name: "API_TOKEN".into(),
            reference: reference.clone(),
        },
    );
    assert!(kept.is_ok());
    remove_secret(root, &reference).unwrap();
    kept = execute(&mut connection, root, Command::Launch { project });
    assert_eq!(
        kept.err().map(|problem| problem.0),
        Some("server_secret_not_found".into())
    );
}

#[test]
fn launching_for_an_unknown_project_is_refused() {
    let (dir, mut connection) = database();
    let unknown = execute(
        &mut connection,
        dir.path(),
        Command::Launch {
            project: ProjectId::new("ghost").unwrap(),
        },
    );
    assert_eq!(
        unknown.err().map(|problem| problem.0),
        Some("plugin_project_unknown".into())
    );
}

#[test]
fn a_stored_skill_description_or_source_with_hidden_characters_is_shown_escaped() {
    let record = SkillRecord {
        id: "s1".into(),
        name: "fmt".into(),
        description: "Formats changed files.\u{e0041}\u{e0042}".into(),
        scope: Scope::Global,
        enabled: true,
        source: "/Users/o/skills/fmt\u{202e}".into(),
        folder: "global/s1".into(),
    };
    let store = tempfile::tempdir().unwrap();
    let skill_md = store.path().join(&record.folder).join("SKILL.md");
    std::fs::create_dir_all(skill_md.parent().unwrap()).unwrap();
    std::fs::write(
        &skill_md,
        format!("---\nname: fmt\ndescription: {}\n---\n", record.description),
    )
    .unwrap();
    let view = skill_view(&record, store.path(), None);
    assert_eq!(
        view.description,
        "Formats changed files.\\u{e0041}\\u{e0042}"
    );
    assert_eq!(view.line, view.description);
    assert_eq!(view.source, "/Users/o/skills/fmt\\u202e");
    for text in [&view.description, &view.line, &view.source] {
        assert!(!crate::memory::hidden_text(text), "{text}");
    }
    assert_eq!(view.refused.as_deref(), Some("skill_description_hidden"));
    assert!(view.enabled, "the owner's switch is shown as it is");

    std::fs::write(
        &skill_md,
        "---\nname: fmt\ndescription: Formats changed files.\n---\n",
    )
    .unwrap();
    assert_eq!(skill_view(&record, store.path(), None).refused, None);
    std::fs::remove_file(&skill_md).unwrap();
    assert_eq!(
        skill_view(&record, store.path(), None).refused.as_deref(),
        Some("skill_unreadable")
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn a_dot_folder_part_never_reaches_the_store() {
    let root = tempfile::tempdir().unwrap();
    let _ = store_folder(root.path(), &["skills", "project", "."]);
}
