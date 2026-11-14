use super::*;
use std::os::unix::fs::PermissionsExt;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    super::super::initialize_authority_schemas(&connection).unwrap();
    let app = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: ProjectId::new("app").unwrap(),
        spec: crate::protocol::ProjectSpec {
            name: "App".into(),
            repositories: vec![],
        },
    })
    .unwrap();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('app', ?1)", [app])
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
            secrets: Vec::new(),
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
    let connection = Connection::open_in_memory().unwrap();
    let view = skill_view(&connection, &record, store.path(), None).unwrap();
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
    assert_eq!(
        view.version, 0,
        "a skill the owner never changed is at version 0"
    );

    std::fs::write(
        &skill_md,
        "---\nname: fmt\ndescription: Formats changed files.\n---\n",
    )
    .unwrap();
    assert_eq!(
        skill_view(&connection, &record, store.path(), None)
            .unwrap()
            .refused,
        None
    );
    std::fs::remove_file(&skill_md).unwrap();
    assert_eq!(
        skill_view(&connection, &record, store.path(), None)
            .unwrap()
            .refused
            .as_deref(),
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
    let version = plugin_version(&connection, &shared).unwrap();
    execute(
        &mut connection,
        root,
        Command::Switch {
            project: project.clone(),
            id: shared.clone(),
            on: false,
            version,
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
            secrets: Vec::new(),
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

    crate::storage::trash::sweep(&connection, root, None).unwrap();
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
    crate::storage::trash::sweep(&connection, root, None).unwrap();
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
            secrets: Vec::new(),
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

    crate::storage::trash::sweep(&connection, root, None).unwrap();
    assert!(!absolute.exists(), "the orphaned skill folder is reclaimed");
    assert!(
        !secret_saved(root, &reference),
        "the orphaned secret file is reclaimed"
    );
}

#[test]
fn recreating_a_project_clears_the_reused_ids_stale_skill_folder_at_once() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let project = ProjectId::new("reused").unwrap();

    connection
        .execute("INSERT INTO projects(id, body) VALUES('reused','{}')", [])
        .unwrap();
    let project_store = root
        .join("plugins")
        .join("skills")
        .join("project")
        .join("reused");
    let stale = project_store.join("old-notes");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(
        stale.join("SKILL.md"),
        "---\nname: old-notes\ndescription: Old project notes.\n---\n",
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let record = SkillRecord {
            id: "skill-old".into(),
            name: "old-notes".into(),
            description: "Old project notes.".into(),
            scope: Scope::Project {
                project_id: project.clone(),
            },
            enabled: true,
            source: "/private/tmp/old-notes".into(),
            folder: Path::new("plugins")
                .join("skills")
                .join("project")
                .join("reused")
                .join("old-notes")
                .to_string_lossy()
                .into_owned(),
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
        .execute("DELETE FROM projects WHERE id='reused'", [])
        .unwrap();

    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::storage::create_project(
            &tx,
            &project,
            &crate::protocol::ProjectSpec {
                name: "Reused".into(),
                repositories: vec![],
            },
        )
        .unwrap();
        assert!(matches!(outcome, crate::protocol::Outcome::Applied));
        tx.commit().unwrap();
    }

    assert!(
        !project_store.exists(),
        "the reused id's stale skill store is cleared at creation, before any trash sweep"
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn a_dot_folder_part_never_reaches_the_store() {
    let root = tempfile::tempdir().unwrap();
    let _ = store_folder(root.path(), &["skills", "project", "."]);
}

/// Grows a list of items made of `unit` until the check stops accepting it: each item as
/// long as the check takes, at most `count` items of `length` units. Inputs: the bounds,
/// the unit and the check. Output: the largest list the check accepts.
fn largest(
    count: usize,
    length: usize,
    unit: &str,
    accepts: &dyn Fn(&[String]) -> bool,
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    while items.len() < count {
        let mut size = length;
        loop {
            items.push(unit.repeat(size));
            if accepts(&items) {
                break;
            }
            items.pop();
            if size == 1 {
                return items;
            }
            size -= 1;
        }
    }
    items
}

#[test]
fn the_largest_server_the_checks_accept_fits_one_reply_frame() {
    const REPLY_ROOM: usize = 256;
    let (dir, mut connection) = database();
    let root = dir.path();
    let command = format!("/{}", "'".repeat(crate::plugins::COMMAND_BYTES_MAX - 1));
    let args = largest(
        crate::plugins::ARGS_MAX,
        crate::plugins::ARG_BYTES_MAX,
        "'",
        &|args| {
            ServerSpec::Stdio {
                command: command.clone(),
                args: args.to_vec(),
            }
            .validate()
            .is_ok()
        },
    );
    let spec = ServerSpec::Stdio { command, args };
    spec.validate().unwrap();
    let id = server(
        &mut connection,
        root,
        &"s".repeat(crate::plugins::SERVER_NAME_BYTES_MAX),
        &spec,
    );
    for index in 0..SECRETS_PER_SERVER_MAX {
        let name = format!(
            "S{index:0>2}{}",
            "_".repeat(crate::plugins::SECRET_NAME_BYTES_MAX - 3)
        );
        attach(&mut connection, root, &id, &name, b"dummy-limit-value");
    }
    let fits = |connection: &mut Connection| {
        let ResultValue::Server(view) =
            execute(connection, root, Command::Server { id: id.clone() }).unwrap()
        else {
            panic!("server view required");
        };
        let ResultValue::Servers(views, _) = execute(
            connection,
            root,
            Command::Servers {
                project: None,
                offset: 0,
            },
        )
        .unwrap() else {
            panic!("server page required");
        };
        assert_eq!(views.len(), 1);
        for bytes in [
            serde_json::to_vec(&view).unwrap().len(),
            serde_json::to_vec(&views).unwrap().len(),
        ] {
            assert!(
                bytes + REPLY_ROOM <= crate::protocol::FRAME_BYTES_MAX,
                "one server's view takes {bytes} bytes"
            );
        }
        *view
    };
    fits(&mut connection);
    let tools = largest(TOOLS_MAX, TOOL_NAME_BYTES_MAX, "\"", &|tools| {
        valid_tools(tools.to_vec()).is_ok()
    });
    assert!(!tools.is_empty());
    execute(
        &mut connection,
        root,
        Command::RecordCheck {
            id: id.clone(),
            digest: spec.digest(),
            outcome: Ok(tools.clone()),
        },
    )
    .unwrap();
    let view = fits(&mut connection);
    assert_eq!(view.tools, tools, "the check kept every tool it accepted");
    assert_eq!(view.secrets.len(), SECRETS_PER_SERVER_MAX);
}

fn tools(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<Vec<ToolView>, String> {
    match execute(connection, root, command) {
        Ok(ResultValue::Tools(views)) => Ok(views),
        Ok(_) => panic!("tool views required"),
        Err(error) => Err(error.0),
    }
}

fn set_tool(
    connection: &mut Connection,
    root: &Path,
    project: Option<&ProjectId>,
    server: Option<&str>,
    tool: &str,
    switch: Switch,
) -> Result<Vec<ToolView>, String> {
    tools(
        connection,
        root,
        Command::SetTool {
            project: project.cloned(),
            server: server.map(str::to_owned),
            tool: tool.into(),
            switch,
        },
    )
}

fn one<'a>(views: &'a [ToolView], tool: &str) -> &'a ToolView {
    views.iter().find(|view| view.tool == tool).unwrap()
}

/// Adds a project row to the plugin test database, as a created project leaves it.
/// Input: the connection and the id. Output: the project id.
fn add_project(connection: &Connection, id: &str) -> ProjectId {
    let project = ProjectId::new(id).unwrap();
    let body = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: project.clone(),
        spec: crate::protocol::ProjectSpec {
            name: id.into(),
            repositories: vec![],
        },
    })
    .unwrap();
    connection
        .execute(
            "INSERT INTO projects(id, body) VALUES(?1, ?2)",
            [id, body.as_str()],
        )
        .unwrap();
    project
}

/// Adds an MCP server in a scope and records a real check of the stdio MCP fixture as its
/// tool list. Inputs: the connection, store root, scope and name. Output: the server id.
fn checked_server(connection: &mut Connection, root: &Path, scope: Scope, name: &str) -> String {
    let path = root.join(format!("{name}-mcp.py"));
    std::fs::write(&path, include_str!("../../tests/fixtures/mcp_fixture.py")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let first = crate::spawn_gate::first_run(
        std::process::Command::new(&path)
            .arg("exit")
            .env_clear()
            .env("PATH", "/usr/bin:/bin"),
    );
    assert_eq!(first.code(), Some(3));
    let command = path.to_str().unwrap().to_owned();
    let spec = ServerSpec::Stdio {
        command: command.clone(),
        args: vec![],
    };
    let ResultValue::Server(view) = execute(
        connection,
        root,
        Command::AddServer {
            scope,
            name: name.into(),
            spec: spec.clone(),
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    let listed =
        crate::plugins::check::check_stdio(&command, &[], &[], std::time::Duration::from_secs(10))
            .unwrap();
    assert_eq!(listed, ["echo", "anonymous"]);
    execute(
        connection,
        root,
        Command::RecordCheck {
            id: view.id.clone(),
            digest: spec.digest(),
            outcome: Ok(listed),
        },
    )
    .unwrap();
    view.id
}

#[test]
fn tool_views_lists_builtins_with_global_and_project_rows() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let app = ProjectId::new("app").unwrap();
    let global = tools(
        &mut connection,
        root,
        Command::Tools {
            project: None,
            server: None,
        },
    )
    .unwrap();
    assert_eq!(
        global
            .iter()
            .map(|view| view.tool.as_str())
            .collect::<Vec<_>>(),
        BUILTIN_TOOLS
            .iter()
            .map(|(tool, _)| *tool)
            .collect::<Vec<_>>()
    );
    assert!(global.iter().all(|view| view.builtin
        && view.server == CRAZYAGENT_SERVER
        && view.server_id.is_none()
        && view.global == Switch::On
        && view.here.is_none()
        && !view.description.is_empty()));

    set_tool(&mut connection, root, None, None, "start_work", Switch::Off).unwrap();
    let set = set_tool(
        &mut connection,
        root,
        Some(&app),
        None,
        "start_work",
        Switch::On,
    )
    .unwrap();
    assert_eq!(one(&set, "start_work").here, Some(Switch::On));

    let global = tools(
        &mut connection,
        root,
        Command::Tools {
            project: None,
            server: None,
        },
    )
    .unwrap();
    let start = one(&global, "start_work");
    assert_eq!((start.global, start.here), (Switch::Off, None));
    let here = tools(
        &mut connection,
        root,
        Command::Tools {
            project: Some(app.clone()),
            server: None,
        },
    )
    .unwrap();
    let start = one(&here, "start_work");
    assert_eq!((start.global, start.here), (Switch::Off, Some(Switch::On)));
    assert!(
        here.iter()
            .filter(|view| view.tool != "start_work")
            .all(|view| view.here.is_none())
    );
    assert_eq!(
        tools(
            &mut connection,
            root,
            Command::Tools {
                project: Some(ProjectId::new("absent").unwrap()),
                server: None,
            },
        )
        .unwrap_err(),
        "plugin_project_unknown"
    );
}

#[test]
fn set_tool_refuses_unknown_tool_and_hidden_mcp_tool() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let id = checked_server(&mut connection, root, Scope::Global, "fixture");
    let listed = tools(
        &mut connection,
        root,
        Command::Tools {
            project: None,
            server: Some(id.clone()),
        },
    )
    .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|view| view.tool.as_str())
            .collect::<Vec<_>>(),
        ["echo", "anonymous"]
    );
    assert!(listed.iter().all(|view| !view.builtin
        && view.server == "fixture"
        && view.server_id.as_deref() == Some(id.as_str())));
    assert_eq!(
        set_tool(
            &mut connection,
            root,
            None,
            Some(&id),
            "echo",
            Switch::Hidden
        )
        .unwrap_err(),
        "tool_delete_builtin_only"
    );
    assert_eq!(
        set_tool(&mut connection, root, None, Some(&id), "nope", Switch::Off).unwrap_err(),
        "tool_unknown"
    );
    assert_eq!(
        set_tool(&mut connection, root, None, None, "nope", Switch::Off).unwrap_err(),
        "tool_unknown"
    );
    let off = set_tool(&mut connection, root, None, Some(&id), "echo", Switch::Off).unwrap();
    assert_eq!(one(&off, "echo").global, Switch::Off);
    assert_eq!(one(&off, "anonymous").global, Switch::On);
    let hidden = set_tool(
        &mut connection,
        root,
        None,
        None,
        "start_work",
        Switch::Hidden,
    )
    .unwrap();
    assert_eq!(one(&hidden, "start_work").global, Switch::Hidden);
}

#[test]
fn project_only_server_is_not_found_from_another_project() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let app = ProjectId::new("app").unwrap();
    let other = add_project(&connection, "other");
    let id = checked_server(
        &mut connection,
        root,
        Scope::Project {
            project_id: app.clone(),
        },
        "local",
    );
    let own = tools(
        &mut connection,
        root,
        Command::Tools {
            project: Some(app.clone()),
            server: Some(id.clone()),
        },
    )
    .unwrap();
    assert_eq!(own.len(), 2);
    for project in [Some(other), None] {
        assert_eq!(
            tools(
                &mut connection,
                root,
                Command::Tools {
                    project: project.clone(),
                    server: Some(id.clone()),
                },
            )
            .unwrap_err(),
            "server_not_found",
            "{project:?}"
        );
    }
}

#[test]
fn restore_tools_clears_only_that_scope() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let app = ProjectId::new("app").unwrap();
    let id = checked_server(&mut connection, root, Scope::Global, "fixture");
    set_tool(&mut connection, root, None, None, "start_work", Switch::Off).unwrap();
    set_tool(
        &mut connection,
        root,
        Some(&app),
        None,
        "start_work",
        Switch::On,
    )
    .unwrap();
    set_tool(
        &mut connection,
        root,
        Some(&app),
        None,
        "finish_work",
        Switch::Hidden,
    )
    .unwrap();
    set_tool(
        &mut connection,
        root,
        Some(&app),
        Some(&id),
        "echo",
        Switch::Off,
    )
    .unwrap();

    let restored = tools(
        &mut connection,
        root,
        Command::RestoreTools {
            project: Some(app.clone()),
            server: None,
        },
    )
    .unwrap();
    assert!(restored.iter().all(|view| view.here.is_none()));
    assert_eq!(one(&restored, "start_work").global, Switch::Off);
    let mcp = tools(
        &mut connection,
        root,
        Command::Tools {
            project: Some(app.clone()),
            server: Some(id.clone()),
        },
    )
    .unwrap();
    assert_eq!(
        one(&mcp, "echo").here,
        Some(Switch::Off),
        "a restore of the built-ins leaves the MCP server's rows"
    );

    let global = tools(
        &mut connection,
        root,
        Command::RestoreTools {
            project: None,
            server: None,
        },
    )
    .unwrap();
    assert!(global.iter().all(|view| view.global == Switch::On));
}

#[test]
fn a_project_server_check_is_refused_when_the_project_network_cannot_be_read() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let spec = ServerSpec::Stdio {
        command: "/bin/true".into(),
        args: vec![],
    };
    let ResultValue::Server(view) = execute(
        &mut connection,
        root,
        Command::AddServer {
            scope: Scope::Project {
                project_id: ProjectId::new("app").unwrap(),
            },
            name: "p".into(),
            spec,
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    crate::sandbox::policy::save(
        &connection,
        "app",
        &crate::sandbox::policy::AgentPolicy::default(),
    )
    .unwrap();
    connection
        .execute(
            "UPDATE project_agent_policy SET body=?1 WHERE project='app'",
            [r#"{"access":"sandboxed","toolchains":[],"network":"proxy"}"#],
        )
        .unwrap();
    let refused = execute(&mut connection, root, Command::GetServer { id: view.id });
    let Err(error) = refused else {
        panic!("the project server check must be refused");
    };
    assert_eq!(error.0, "project_network_unreadable");
}

#[test]
fn a_global_server_check_uses_the_strictest_network_among_projects_that_enable_it() {
    use crate::sandbox::policy::{AgentNetwork, AgentPolicy};
    let (dir, mut connection) = database();
    let root = dir.path();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('beta','{}')", [])
        .unwrap();
    crate::sandbox::policy::save(
        &connection,
        "app",
        &AgentPolicy {
            network: AgentNetwork::Open,
            ..AgentPolicy::default()
        },
    )
    .unwrap();
    crate::sandbox::policy::save(
        &connection,
        "beta",
        &AgentPolicy {
            network: AgentNetwork::None,
            ..AgentPolicy::default()
        },
    )
    .unwrap();
    let spec = ServerSpec::Stdio {
        command: "/bin/true".into(),
        args: vec![],
    };
    let id = server(&mut connection, root, "fixture", &spec);
    let ResultValue::Check(_, _, network) =
        execute(&mut connection, root, Command::GetServer { id }).unwrap()
    else {
        panic!("check value required");
    };
    assert_eq!(network, crate::sandbox::SandboxNetwork::Only(vec![]));
}
#[test]
fn a_case_variant_project_id_is_refused_and_keeps_the_first_projects_skills() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let first = ProjectId::new("Proj-a").unwrap();
    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::storage::create_project(
            &tx,
            &first,
            &crate::protocol::ProjectSpec {
                name: "Proj A".into(),
                repositories: vec![],
            },
        )
        .unwrap();
        assert!(matches!(outcome, crate::protocol::Outcome::Applied));
        tx.commit().unwrap();
    }
    let store = root
        .join("plugins")
        .join("skills")
        .join("project")
        .join("Proj-a");
    std::fs::create_dir_all(store.join("notes")).unwrap();
    std::fs::write(
        store.join("notes").join("SKILL.md"),
        "---\nname: notes\ndescription: Live notes.\n---\n",
    )
    .unwrap();
    {
        let tx = connection.transaction().unwrap();
        let record = SkillRecord {
            id: "skill-live".into(),
            name: "notes".into(),
            description: "Live notes.".into(),
            scope: Scope::Project {
                project_id: first.clone(),
            },
            enabled: true,
            source: "/private/tmp/notes".into(),
            folder: Path::new("plugins")
                .join("skills")
                .join("project")
                .join("Proj-a")
                .join("notes")
                .to_string_lossy()
                .into_owned(),
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

    let variant = ProjectId::new("proj-a").unwrap();
    {
        let tx = connection.transaction().unwrap();
        let outcome = crate::storage::create_project(
            &tx,
            &variant,
            &crate::protocol::ProjectSpec {
                name: "proj a".into(),
                repositories: vec![],
            },
        )
        .unwrap();
        assert!(
            matches!(&outcome, crate::protocol::Outcome::Rejected { code } if code == "project_id_case_conflict"),
            "{outcome:?}"
        );
        tx.commit().unwrap();
    }

    assert!(
        store.join("notes").exists(),
        "the first project's skill store survives a refused case variant"
    );
}

#[test]
fn removing_a_dot_skill_folder_is_refused_and_never_deletes_the_project_root() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("plugins").join("skills").join("project");
    std::fs::create_dir_all(base.join("real")).unwrap();
    assert!(remove_skill_project_folder(root.path(), ".").is_err());
    assert!(
        base.join("real").exists(),
        "a dot id never reaches remove_dir_all on the shared project root"
    );
}

#[test]
fn a_colon_project_s_skill_folder_is_still_swept_off_windows_and_a_device_name_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("plugins").join("skills").join("project");
    std::fs::create_dir_all(base.join("name:x")).unwrap();
    std::fs::write(base.join("name:x").join("SKILL.md"), "stale").unwrap();
    std::fs::create_dir_all(base.join("name")).unwrap();
    std::fs::write(base.join("name").join("SKILL.md"), "kept").unwrap();
    let swept = remove_skill_project_folder(root.path(), "name:x");
    assert!(
        matches!(swept, Ok(())) && !base.join("name:x").exists(),
        "a project id may hold a colon, and create and the trash sweep still clear its folder here: {swept:?}"
    );
    let device = remove_skill_project_folder(root.path(), "com1:");
    assert!(
        matches!(&device, Err(refused) if refused.0 == "plugin_skill_folder_invalid"),
        "{device:?}"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("name").join("SKILL.md")).unwrap(),
        "kept"
    );
}

#[test]
fn an_mcp_secret_shorter_than_eight_bytes_is_refused_with_a_plain_reason() {
    let (dir, _connection) = database();
    let root = dir.path();
    for value in [&b"1"[..], b"a", b"true", b"1234567"] {
        let reference = new_secret_reference().unwrap();
        assert_eq!(
            store_secret(root, &reference, value.to_vec())
                .err()
                .map(|problem| problem.0)
                .as_deref(),
            Some("server_secret_too_short"),
            "a {}-byte secret is a substring of launch paths and flags",
            value.len()
        );
        assert_eq!(
            read_secret(root, &reference)
                .err()
                .map(|problem| problem.0)
                .as_deref(),
            Some("server_secret_not_found"),
            "a refused secret leaves no file"
        );
    }
    assert_eq!(
        crate::plugins::known_code("server_secret_too_short"),
        Some("server_secret_too_short"),
        "the desktop shows only codes it knows"
    );
    let reason = crate::plugins::plain_problem("server_secret_too_short").unwrap();
    assert!(reason.contains("8 characters"), "{reason}");
    let reference = new_secret_reference().unwrap();
    store_secret(root, &reference, b"12345678".to_vec()).unwrap();
    assert_eq!(read_secret(root, &reference).unwrap(), b"12345678");
}

/// Where the engine has no shared context-mode install, the default pack's pinned
/// context-mode launch is left out with its reason, never run through npx, and every other
/// spec runs as approved.
#[test]
fn a_launch_without_a_shared_install_leaves_the_pinned_context_mode_server_out_never_npx() {
    use crate::plugins::ContextModeOff;
    let stdio = |command: &str, package: &str| ServerSpec::Stdio {
        command: command.into(),
        args: vec!["-y".into(), package.into()],
    };
    assert_eq!(
        without_shared_install(stdio("/opt/homebrew/bin/npx", "context-mode@1.0.169")),
        Err(ContextModeOff::Unsupported)
    );
    for kept in [
        stdio("/opt/homebrew/bin/npx", "context-mode@latest"),
        stdio("/opt/homebrew/bin/npx", "other-server@1.0.0"),
        stdio("npx", "context-mode@1.0.169"),
        stdio("/opt/homebrew/bin/node", "context-mode@1.0.169"),
    ] {
        assert_eq!(without_shared_install(kept.clone()), Ok(kept));
    }
}
