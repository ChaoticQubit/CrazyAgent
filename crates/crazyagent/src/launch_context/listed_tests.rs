use super::*;
use crate::harnesses::manifest::parse;
use crate::plugins::SecretValue;

#[path = "listed_walk_tests.rs"]
mod walk_tests;

const SMALL: &str = "schema: 1\nkind: harness\nid: \"sample\"\nname: \"Sample\"\nbinaries:\n  - \"sample\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\n";

const BLOCKS: &str = "home:\n  env_prefixes:\n    - \"SAMPLE_\"\n  env:\n    SAMPLE_CONFIG: \"{home}/agent\"\n    SAMPLE_UPDATES: \"off\"\n  set_home: true\nmcp:\n  render: \"json-mcp-servers\"\n  path: \"{home}/agent/mcp.json\"\ninstructions:\n  render: \"file\"\n  path: \"{home}/agent/AGENTS.md\"\n";

const INSTRUCTIONS: &str = "# Project context from CrazyAgent\n";

fn manifest(blocks: &str) -> crate::harnesses::manifest::Manifest {
    parse(format!("{SMALL}{blocks}").as_bytes()).unwrap()
}

/// A listed launch's private home as T18 lays it out, `<agent home>/<harness id>/account-1`,
/// each folder 0700. Input: the harness id. Output: the agent home's guard and the private
/// home.
fn harness_home(id: &str) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let base = folder.path().canonicalize().unwrap();
    let home = base.join(id).join("account-1");
    fs::create_dir_all(&home).unwrap();
    for path in [base.clone(), base.join(id), home.clone()] {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    (folder, home)
}

/// The private home [`harness_home`] lays out for the sample manifest's harness.
/// Output: the agent home's guard and the private home.
fn private_home() -> (tempfile::TempDir, PathBuf) {
    harness_home("sample")
}

fn context() -> AgentContext {
    AgentContext {
        plugins: LaunchPlugins {
            servers: vec![LaunchServer {
                name: "files".into(),
                spec: ServerSpec::Stdio {
                    command: "/usr/bin/files".into(),
                    args: vec!["--stdio".into()],
                },
                secrets: vec![LaunchSecret {
                    name: "FILES_TOKEN".into(),
                    value: SecretValue::new(b"s3cret".to_vec()),
                }],
            }],
            ..LaunchPlugins::default()
        },
        instructions: INSTRUCTIONS.into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        native_threads: None,
        relay: None,
        model_proxy: None,
        freeze_record: None,
        terminal_display: TerminalDisplay::Classic,
        catch_up_main: true,
    }
}

fn servers_in(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&text).ok()
}

/// Task 18: a listed harness's launch writes the MCP servers and the instruction text where
/// its manifest says, hands over the environment its home block sets, records the files on
/// its guard for a later engine, and deletes them once the launch ends.
#[test]
fn a_listed_harness_launch_writes_the_files_and_environment_its_manifest_asks_for() {
    let (_folder, home) = private_home();
    let mcp = home.join("agent").join("mcp.json");
    let instructions = home.join("agent").join("AGENTS.md");

    let installed = context().install_listed(&manifest(BLOCKS), &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    assert_eq!(
        servers_in(&mcp),
        Some(serde_json::json!({
            "mcpServers": {
                "files": {
                    "type": "stdio",
                    "command": "/usr/bin/files",
                    "args": ["--stdio"],
                    "env": { "FILES_TOKEN": "s3cret" }
                }
            }
        }))
    );
    let text = fs::read_to_string(&instructions).unwrap_or_default();
    assert!(text.starts_with(INSTRUCTIONS), "{text}");
    let installed = installed.ok();
    assert_eq!(
        installed.as_ref().map(|found| found.environment().to_vec()),
        Some(vec![
            (
                OsString::from("SAMPLE_CONFIG"),
                home.join("agent").into_os_string()
            ),
            (OsString::from("SAMPLE_UPDATES"), OsString::from("off")),
            (OsString::from("HOME"), home.clone().into_os_string()),
        ])
    );
    assert_eq!(
        installed
            .as_ref()
            .and_then(|found| found.files())
            .map(|files| files.rendered.clone()),
        Some(vec![
            "agent/mcp.json".to_owned(),
            "agent/AGENTS.md".to_owned()
        ])
    );
    assert_eq!(
        installed.as_ref().map(|found| found.arguments().len()),
        Some(0)
    );

    drop(installed);

    assert!(fs::symlink_metadata(&mcp).is_err());
    assert!(fs::symlink_metadata(&instructions).is_err());
}

/// Task 18: with tools off a listed launch gives no MCP server, and the MCP file an earlier
/// launch left with servers in it is replaced by one that holds none.
#[test]
fn with_tools_off_a_listed_launch_replaces_an_earlier_server_file_with_an_empty_one() {
    let (_folder, home) = private_home();
    fs::create_dir(home.join("agent")).unwrap();
    fs::set_permissions(home.join("agent"), fs::Permissions::from_mode(0o700)).unwrap();
    let mcp = home.join("agent").join("mcp.json");
    fs::write(&mcp, "{\"mcpServers\":{\"stale\":{\"type\":\"http\"}}}").unwrap();

    let installed = context().install_listed(&manifest(BLOCKS), &home, false);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    assert_eq!(
        servers_in(&mcp),
        Some(serde_json::json!({ "mcpServers": {} }))
    );
}

/// Task 18: the MCP file a listed harness reads cannot switch off one tool of a server, so
/// a server with any tool the owner switched off is left out whole, never given with that
/// tool on; a server with nothing switched off is given.
#[test]
fn a_listed_launch_leaves_out_a_server_with_a_tool_the_owner_switched_off() {
    let (_folder, home) = private_home();
    let mut context = context();
    context.plugins.servers.push(LaunchServer {
        name: "notes".into(),
        spec: ServerSpec::Http {
            url: "http://127.0.0.1:4100/mcp".into(),
        },
        secrets: Vec::new(),
    });
    context.off_tools = vec![
        ("files".to_owned(), vec!["delete".to_owned()]),
        ("notes".to_owned(), Vec::new()),
    ];

    let installed = context.install_listed(&manifest(BLOCKS), &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    assert_eq!(
        servers_in(&home.join("agent").join("mcp.json")),
        Some(serde_json::json!({
            "mcpServers": {
                "notes": {
                    "type": "http",
                    "url": "http://127.0.0.1:4100/mcp",
                    "headers": {}
                }
            }
        }))
    );
}

/// Task 18: a manifest whose blocks do not read starts no launch and writes nothing into
/// the private home.
#[test]
fn a_listed_launch_refuses_a_manifest_whose_blocks_do_not_read_and_writes_nothing() {
    let (_folder, home) = private_home();
    let blocks = "mcp:\n  render: \"yaml-servers\"\n  path: \"{home}/mcp.yaml\"\n";

    let refused = context().install_listed(&manifest(blocks), &home, true);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_manifest_invalid".to_owned())
    );
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
}

/// T18 r2 F3 (rv-code4 A4): a team agent's launch carries project instruction files only
/// the Codex and Claude Code homes know where to put, so a listed harness refuses it with
/// `launch_harness_unsupported` and writes nothing into the private home.
#[test]
fn a_team_agent_launch_of_a_listed_harness_is_refused_and_writes_nothing() {
    let (_folder, home) = private_home();
    let project = tempfile::tempdir().unwrap();
    let mut context = context();
    context.process = Some(Box::new(Process {
        project: crate::agent_home::AgentProject {
            data: project.path().join("data"),
            id: ProjectId::new("listed").unwrap(),
            folder: Some(project.path().join("project")),
            mode: crate::instruction_files::Mode::Both,
            repeated: crate::agent_home::Repeated::LeaveOut,
        },
        hooks: Vec::new(),
        agent: String::new(),
    }));

    let refused = context.install_listed(&manifest(BLOCKS), &home, true);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_harness_unsupported".to_owned())
    );
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
}

/// Task 18: an engine that adopts a pane deletes the files a listed harness's manifest had
/// written, and only those: a path in the record that leaves the home, names a folder or
/// runs through a link planted in the home is left alone, and so is what the link names.
#[test]
fn an_adopted_listed_launch_removes_only_its_own_files_inside_the_home() {
    let (_folder, home) = private_home();
    let outside = tempfile::tempdir().unwrap();
    let kept = outside.path().join("mcp.json");
    fs::write(&kept, "kept").unwrap();
    let beside = home.with_file_name("beside.json");
    fs::write(&beside, "kept").unwrap();
    fs::create_dir(home.join("agent")).unwrap();
    fs::write(home.join("agent").join("mcp.json"), "{}").unwrap();
    std::os::unix::fs::symlink(outside.path(), home.join("linked")).unwrap();
    let files = InstalledFiles {
        home: home.clone(),
        codex: false,
        rendered: vec![
            "agent/mcp.json".to_owned(),
            "../beside.json".to_owned(),
            "agent".to_owned(),
            "linked/mcp.json".to_owned(),
        ],
        listed: true,
    };

    let adopted = Installed::adopt(&files);
    assert!(adopted.is_ok(), "{:?}", adopted.as_ref().err());
    drop(adopted);

    assert!(fs::symlink_metadata(home.join("agent").join("mcp.json")).is_err());
    assert!(home.join("agent").is_dir());
    assert_eq!(fs::read_to_string(&kept).ok().as_deref(), Some("kept"));
    assert_eq!(fs::read_to_string(&beside).ok().as_deref(), Some("kept"));
}
