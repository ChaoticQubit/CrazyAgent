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
