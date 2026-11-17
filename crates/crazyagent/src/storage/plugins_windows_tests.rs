/// V243-WIN-MCP-NPX and #341 sec F1: an approved server whose command is a Windows batch file
/// other than npm's npx.cmd, or a name with no extension or a dotted one with a `.cmd` beside
/// it, is left out of a launch, by name in `batch_files` and `left_out`, while a server whose
/// command is a program runs as approved.
#[test]
fn a_batch_file_server_is_left_out_of_a_launch_by_name() {
    use super::{Command, ResultValue, execute};
    use crate::plugins::{Scope, ServerSpec};
    let directory = tempfile::tempdir().unwrap();
    let root = crate::sandbox::canonical(directory.path()).unwrap();
    let mut connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let app = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: crate::protocol::ProjectId::new("app").unwrap(),
        spec: crate::protocol::ProjectSpec {
            name: "App".into(),
            repositories: vec![],
        },
    })
    .unwrap();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('app', ?1)", [app])
        .unwrap();
    for name in ["run.bat", "tool.cmd", "tool.v2.cmd"] {
        std::fs::write(root.join(name), "@ECHO off\r\n").unwrap();
    }
    let program = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    let servers = [
        ("notes", root.join("run.bat")),
        ("bare", root.join("tool")),
        ("dotted", root.join("tool.v2")),
        ("lookup", program),
    ];
    for (name, command) in servers {
        let spec = ServerSpec::Stdio {
            command: command.to_str().unwrap().to_owned(),
            args: Vec::new(),
        };
        let ResultValue::Server(view) = execute(
            &mut connection,
            &root,
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
        execute(
            &mut connection,
            &root,
            Command::Approve { id: view.id, spec },
        )
        .unwrap();
    }
    let ResultValue::Launch(launch) = execute(
        &mut connection,
        &root,
        Command::Launch {
            project: crate::protocol::ProjectId::new("app").unwrap(),
        },
    )
    .unwrap() else {
        panic!("launch set required");
    };
    let sorted = |names: &[String]| {
        let mut names = names.to_vec();
        names.sort();
        names
    };
    assert_eq!(sorted(&launch.batch_files), ["bare", "dotted", "notes"]);
    assert_eq!(sorted(&launch.left_out), ["bare", "dotted", "notes"]);
    assert_eq!(launch.context_mode_off, None);
    let names: Vec<&str> = launch.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["lookup"]);
}

#[test]
fn a_skill_folder_name_with_a_colon_is_refused_on_windows() {
    let root = tempfile::tempdir().unwrap();
    for id in ["name:x", "owner:abc"] {
        let outcome = super::remove_skill_project_folder(root.path(), id);
        assert!(
            matches!(&outcome, Err(refused) if refused.0 == "plugin_skill_folder_invalid"),
            "{id}: Windows reads a colon as a stream or device, never as this folder: {outcome:?}"
        );
    }
}
