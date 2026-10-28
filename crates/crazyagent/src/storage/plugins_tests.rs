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
fn removing_a_project_clears_its_plugins_so_a_reused_id_inherits_nothing() {
    let (dir, mut connection) = database();
    let root = dir.path();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('reused','{}')", [])
        .unwrap();
    let project = ProjectId::new("reused").unwrap();
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec![],
    };

    let shared = server(&mut connection, root, "shared", &spec);
    execute(
        &mut connection,
        root,
        Command::Approve {
            id: shared.clone(),
            spec: spec.clone(),
        },
    )
    .unwrap();
    execute(
        &mut connection,
        root,
        Command::Switch {
            project: project.clone(),
            id: shared.clone(),
            on: false,
        },
    )
    .unwrap();

    let ResultValue::Server(view) = execute(
        &mut connection,
        root,
        Command::AddServer {
            scope: Scope::Project {
                project_id: project.clone(),
            },
            name: "private".into(),
            spec: spec.clone(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    let private = view.id;
    attach(
        &mut connection,
        root,
        &private,
        "TOKEN",
        b"super-secret-value",
    );
    execute(
        &mut connection,
        root,
        Command::Approve {
            id: private.clone(),
            spec: spec.clone(),
        },
    )
    .unwrap();
    let reference = {
        let tx = connection.transaction().unwrap();
        let record: ServerRecord = load(&tx, "server", &private).unwrap();
        record.secrets[0].reference.clone()
    };

    let skill_folder = Path::new("plugins")
        .join("skills")
        .join("project")
        .join("reused")
        .join("notes");
    let absolute = root.join(&skill_folder);
    std::fs::create_dir_all(&absolute).unwrap();
    std::fs::write(
        absolute.join("SKILL.md"),
        "---\nname: notes\ndescription: Project notes.\n---\n",
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let record = SkillRecord {
            id: "skill-reused".into(),
            name: "notes".into(),
            description: "Project notes.".into(),
            scope: Scope::Project {
                project_id: project.clone(),
            },
            enabled: true,
            source: "/private/tmp/notes".into(),
            folder: skill_folder.to_string_lossy().into_owned(),
        };
        save(
            &tx,
            "skill",
            &record.id,
            &record.scope,
            &record.name,
            &record,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    {
        let tx = connection.transaction().unwrap();
        let before = effective(&tx, root, &project).unwrap();
        assert_eq!(
            before
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            ["private"],
            "the project sees only its own server; the shared one is switched off"
        );
        assert_eq!(before.servers[0].secrets.len(), 1);
        assert_eq!(
            before
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            ["notes"]
        );
    }
    assert!(absolute.is_dir(), "the project skill folder is on disk");
    assert!(
        secret_saved(root, &reference),
        "the project secret file is on disk"
    );

    let config = crate::config::resolve_config_bytes("", "").unwrap();
    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::project_setup::execute(
            &tx,
            &config,
            &crate::project_setup::SetupAction::Remove {
                project_id: project.clone(),
            },
            2,
        )
        .unwrap();
        assert!(matches!(outcome, crate::protocol::Outcome::Applied));
        tx.commit().unwrap();
    }

    connection
        .execute("INSERT INTO projects(id, body) VALUES('reused','{}')", [])
        .unwrap();
    let items: i64 = connection
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE scope='project:reused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(items, 0, "the project's own items are gone");
    let switches: i64 = connection
        .query_row(
            "SELECT count(*) FROM plugin_switches WHERE project='reused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(switches, 0, "the project's switches are gone");
    assert!(
        absolute.is_dir(),
        "the skill folder waits for the post-commit sweep, not an in-transaction delete"
    );

    {
        let tx = connection.transaction().unwrap();
        let after = effective(&tx, root, &project).unwrap();
        assert!(after.skills.is_empty(), "no skill is inherited");
        assert_eq!(
            after
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            ["shared"],
            "the shared server is visible again and the project server is gone"
        );
    }

    crate::storage::trash::sweep(&connection, root).unwrap();
    assert!(
        !absolute.exists(),
        "the project skill folder is reclaimed after the sweep"
    );
    assert!(
        !secret_saved(root, &reference),
        "the project secret file is reclaimed"
    );
}

#[test]
fn a_rolled_back_removal_keeps_the_project_skill_folder() {
    let (dir, mut connection) = database();
    let root = dir.path();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('victim','{}')", [])
        .unwrap();
    let project = ProjectId::new("victim").unwrap();

    let skill_folder = Path::new("plugins")
        .join("skills")
        .join("project")
        .join("victim")
        .join("notes");
    let absolute = root.join(&skill_folder);
    std::fs::create_dir_all(&absolute).unwrap();
    std::fs::write(
        absolute.join("SKILL.md"),
        "---\nname: notes\ndescription: Project notes.\n---\n",
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let record = SkillRecord {
            id: "skill-victim".into(),
            name: "notes".into(),
            description: "Project notes.".into(),
            scope: Scope::Project {
                project_id: project.clone(),
            },
            enabled: true,
            source: "/private/tmp/notes".into(),
            folder: skill_folder.to_string_lossy().into_owned(),
        };
        save(
            &tx,
            "skill",
            &record.id,
            &record.scope,
            &record.name,
            &record,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let config = crate::config::resolve_config_bytes("", "").unwrap();
    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::project_setup::execute(
            &tx,
            &config,
            &crate::project_setup::SetupAction::Remove {
                project_id: project.clone(),
            },
            2,
        )
        .unwrap();
        assert!(matches!(outcome, crate::protocol::Outcome::Applied));
        drop(tx);
    }

    assert!(
        absolute.is_dir(),
        "a removal rolled back before commit leaves the skill folder on disk"
    );
    crate::storage::trash::sweep(&connection, root).unwrap();
    assert!(
        absolute.is_dir(),
        "the rolled back removal queued no trash, so the sweep keeps the folder"
    );
}

#[test]
fn creating_a_project_clears_plugins_orphaned_by_an_earlier_removal() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let project = ProjectId::new("ghost").unwrap();
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec![],
    };

    connection
        .execute("INSERT INTO projects(id, body) VALUES('ghost','{}')", [])
        .unwrap();

    let ResultValue::Server(view) = execute(
        &mut connection,
        root,
        Command::AddServer {
            scope: Scope::Project {
                project_id: project.clone(),
            },
            name: "private".into(),
            spec: spec.clone(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    let private = view.id;
    attach(
        &mut connection,
        root,
        &private,
        "TOKEN",
        b"super-secret-value",
    );
    execute(
        &mut connection,
        root,
        Command::Approve {
            id: private.clone(),
            spec: spec.clone(),
        },
    )
    .unwrap();
    let reference = {
        let tx = connection.transaction().unwrap();
        let record: ServerRecord = load(&tx, "server", &private).unwrap();
        record.secrets[0].reference.clone()
    };

    let skill_folder = Path::new("plugins")
        .join("skills")
        .join("project")
        .join("ghost")
        .join("notes");
    let absolute = root.join(&skill_folder);
    std::fs::create_dir_all(&absolute).unwrap();
    std::fs::write(
        absolute.join("SKILL.md"),
        "---\nname: notes\ndescription: Project notes.\n---\n",
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let record = SkillRecord {
            id: "skill-ghost".into(),
            name: "notes".into(),
            description: "Project notes.".into(),
            scope: Scope::Project {
                project_id: project.clone(),
            },
            enabled: true,
            source: "/private/tmp/notes".into(),
            folder: skill_folder.to_string_lossy().into_owned(),
        };
        save(
            &tx,
            "skill",
            &record.id,
            &record.scope,
            &record.name,
            &record,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    connection
        .execute("DELETE FROM projects WHERE id='ghost'", [])
        .unwrap();

    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::storage::create_project(
            &tx,
            &project,
            &crate::protocol::ProjectSpec {
                name: "Ghost".into(),
                repositories: vec![],
            },
        )
        .unwrap();
        assert!(matches!(outcome, crate::protocol::Outcome::Applied));

        let after = effective(&tx, root, &project).unwrap();
        assert!(after.skills.is_empty(), "no orphaned skill is inherited");
        assert!(
            after.servers.iter().all(|server| server.name != "private"),
            "no orphaned server is inherited"
        );
        tx.commit().unwrap();
    }

    let items: i64 = connection
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE scope='project:ghost'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(items, 0, "the orphaned rows are gone");

    crate::storage::trash::sweep(&connection, root).unwrap();
    assert!(!absolute.exists(), "the orphaned skill folder is reclaimed");
    assert!(
        !secret_saved(root, &reference),
        "the orphaned secret file is reclaimed"
    );
}
