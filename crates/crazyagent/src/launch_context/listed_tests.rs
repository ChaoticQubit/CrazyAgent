use super::*;
use crate::harnesses::manifest::parse;
use crate::plugins::SecretValue;

#[path = "listed_walk_tests.rs"]
mod walk_tests;

#[path = "listed_pane_tests.rs"]
mod pane_tests;

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
                engine: false,
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
        handover: None,
        tool_home: None,
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
        engine: false,
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

/// T18 r2 F3 (rv-code4 A4), under fix8 Q12 A: a team agent's owner text now goes into the
/// listed harness's instruction file, but its hooks have nowhere to go, so a tools-on team
/// agent whose project turned hooks on is refused before anything is written into the
/// private home.
#[test]
fn a_team_agent_launch_of_a_listed_harness_refused_for_its_hooks_writes_nothing() {
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
        hooks: vec![EffectiveHook {
            plugin: "guard".into(),
            event: "PreToolUse".into(),
            matcher: None,
            command: "/usr/bin/true".into(),
            timeout: None,
            folder: PathBuf::from("/missing-hook-folder"),
        }],
        agent: String::new(),
        caches: crate::agent_home::Caches::default(),
    }));

    let refused = context.install_listed(&manifest(BLOCKS), &home, true);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_listed_hooks_unsupported".to_owned())
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

const ENGINE: &str = "/Applications/CrazyAgent.app/Contents/MacOS/crazyagent";

/// T19b Q2 (leader 19:35): an ACP launch hands its agent in `session/new` only the engine's
/// own team server, marked where the engine adds it; the frame never carries an owner
/// server, its secret name or its secret value.
#[test]
fn an_acp_session_opens_with_the_engines_team_server_and_never_an_owner_secret() {
    let (_folder, home) = private_home();
    let mailbox = home.join("mailbox");
    let context = context()
        .with_agent_team_tool(Path::new(ENGINE), &mailbox, &[], false)
        .unwrap();

    let installed = context.install_listed(&manifest(""), &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    let installed = installed.unwrap();
    let mailbox = mailbox.to_str().unwrap().to_owned();
    assert_eq!(
        installed.engine_servers().to_vec(),
        vec![EngineServer {
            name: crate::team::tool::SERVER_NAME.into(),
            command: ENGINE.into(),
            args: vec![
                crate::team::tool::AGENT_SERVER_COMMAND.into(),
                mailbox.clone()
            ],
        }]
    );
    let (acp, _) =
        crate::harnesses::drivers::acp::Acp::new(Path::new("/work"), true, None).unwrap();
    let mut acp = acp.with_servers(installed.engine_servers());
    let (_, writes) = acp
        .receive(
            br#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{}}}"#,
        )
        .unwrap();
    assert_eq!(writes.len(), 1);
    let frame = String::from_utf8(writes[0].clone()).unwrap();
    let opened: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(
        opened["params"]["mcpServers"],
        serde_json::json!([{
            "name": crate::team::tool::SERVER_NAME,
            "command": ENGINE,
            "args": [crate::team::tool::AGENT_SERVER_COMMAND, mailbox],
            "env": []
        }])
    );
    for owner in ["s3cret", "FILES_TOKEN", "/usr/bin/files", "\"files\""] {
        assert!(
            !frame.contains(owner),
            "{owner} reached session/new: {frame}"
        );
    }
}

/// T19b Q2: a server the owner saved under the engine's team server name is never marked
/// as the engine's own, so it never reaches an ACP session, even with no secret.
#[test]
fn an_owner_server_under_the_team_server_name_never_reaches_an_acp_session() {
    let (_folder, home) = private_home();
    let mut context = context();
    context.plugins.servers.push(LaunchServer {
        name: crate::team::tool::SERVER_NAME.into(),
        engine: false,
        spec: ServerSpec::Stdio {
            command: "/usr/bin/files".into(),
            args: vec![
                crate::team::tool::AGENT_SERVER_COMMAND.into(),
                "/tmp/mailbox".into(),
            ],
        },
        secrets: Vec::new(),
    });

    let installed = context.install_listed(&manifest(""), &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    assert_eq!(
        installed.ok().map(|found| found.engine_servers().to_vec()),
        Some(Vec::new())
    );
}

/// T19b 19b-1: an ACP launch inside a project starts its harness with the manifest's
/// arguments and the variables its home block sets, and keeps the engine's team server for
/// its session, never in an argument.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_in_a_project_starts_with_its_manifest_arguments_and_home_variables() {
    let (_folder, home) = harness_home("standin-acp");
    let work = tempfile::tempdir().unwrap();
    let work = work.path().canonicalize().unwrap();
    let profiles = tempfile::tempdir().unwrap();
    let profile = profiles.path().canonicalize().unwrap().join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = context()
        .with_agent_team_tool(Path::new(ENGINE), &home.join("mailbox"), &[], false)
        .unwrap();
    let request = crate::harnesses::launch::LaunchRequest {
        harness: "standin-acp".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: work,
        account: crate::harnesses::launch::AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: crate::harnesses::launch::LaunchLimits::default(),
    };

    let built = crate::harnesses::launch::command_with_context(&request, &context);

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let (command, installed) = built.unwrap();
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    assert_eq!(
        args[args.len().saturating_sub(2)..],
        [std::ffi::OsStr::new("--home"), home.as_os_str()]
    );
    assert!(
        args.iter()
            .all(|argument| !argument.to_string_lossy().contains("team-mcp")),
        "{args:?}"
    );
    let config = command
        .get_envs()
        .find(|(name, _)| *name == "STANDIN_ACP_CONFIG")
        .and_then(|(_, value)| value);
    assert_eq!(config, Some(home.join("config").as_os_str()));
    assert_eq!(installed.engine_servers().len(), 1);
}

const ACP_PIPED: &str = "piped:\n  driver: \"acp\"\n  argv:\n    - \"--home\"\n    - \"{home}\"\n  tools_off:\n    - \"--no-tools\"\n";

fn rendered_names(home: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = servers_in(&home.join("agent").join("mcp.json"))?["mcpServers"]
        .as_object()?
        .keys()
        .cloned()
        .collect();
    names.sort();
    Some(names)
}

/// T19b 19b-1: an ACP harness takes the engine's team server in `session/new`, so the MCP
/// file its manifest renders names only the owner's servers and the agent never sees the team
/// server twice; any other listed harness still finds the team server in that file.
#[test]
fn an_acp_harness_renders_only_the_owners_servers_and_takes_the_team_server_in_its_session() {
    let (_folder, home) = private_home();
    let teamed = || {
        context()
            .with_agent_team_tool(Path::new(ENGINE), &home.join("mailbox"), &[], false)
            .unwrap()
    };

    let acp = teamed().install_listed(&manifest(&format!("{ACP_PIPED}{BLOCKS}")), &home, true);

    assert!(acp.is_ok(), "{:?}", acp.as_ref().err());
    assert_eq!(rendered_names(&home), Some(vec!["files".to_owned()]));
    assert_eq!(acp.ok().map(|found| found.engine_servers().len()), Some(1));

    let other = teamed().install_listed(&manifest(BLOCKS), &home, true);

    assert!(other.is_ok(), "{:?}", other.as_ref().err());
    assert_eq!(
        rendered_names(&home),
        Some(vec![
            crate::team::tool::SERVER_NAME.to_owned(),
            "files".to_owned()
        ])
    );
}

/// T19b 19b-3: a started ACP agent's session opens with the engine's team server its
/// installed context carries, as the stand-in's own log of its input shows, and with no owner
/// secret; this covers the line that hands those servers from the launch to its driver.
#[cfg(target_os = "macos")]
#[test]
fn a_started_acp_agent_opens_its_session_with_the_engines_team_server_and_no_owner_secret() {
    let (_folder, home) = harness_home("standin-acp");
    let base = tempfile::Builder::new()
        .prefix("ca-acp-team-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let work = base.path().canonicalize().unwrap().join("work");
    fs::DirBuilder::new().mode(0o700).create(&work).unwrap();
    let profile = base.path().canonicalize().unwrap().join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let install = tempfile::tempdir().unwrap();
    let program = install.path().canonicalize().unwrap().join("standin-acp");
    fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/harness/standin.sh"
        ),
        &program,
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let mailbox = home.join("mailbox");
    let context = context()
        .with_agent_team_tool(Path::new(ENGINE), &mailbox, &[], false)
        .unwrap();
    let request = crate::harnesses::launch::LaunchRequest {
        harness: "standin-acp".into(),
        executable: program,
        working_dir: work,
        account: crate::harnesses::launch::AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: crate::harnesses::launch::LaunchLimits::default(),
    };

    let started = crate::harnesses::launch::Session::start_with_context(request, context);

    assert!(started.is_ok(), "{:?}", started.as_ref().err());
    let mut session = started.unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, crate::harnesses::launch::LaunchEvent::Ready { .. }))
    {
        assert!(std::time::Instant::now() < deadline, "{events:?}");
        if let Some(event) = session
            .next_event(std::time::Duration::from_millis(200))
            .unwrap()
        {
            events.push(event);
        }
    }
    let log = fs::read_to_string(home.join("standin").join("stdin-1")).unwrap_or_default();
    let opened = log
        .lines()
        .find(|line| line.contains("\"session/new\""))
        .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok());
    assert_eq!(
        opened.map(|frame| frame["params"]["mcpServers"].clone()),
        Some(serde_json::json!([{
            "name": crate::team::tool::SERVER_NAME,
            "command": ENGINE,
            "args": [crate::team::tool::AGENT_SERVER_COMMAND, mailbox.to_str().unwrap()],
            "env": []
        }])),
        "{log}"
    );
    assert!(
        !log.contains("s3cret") && !log.contains("FILES_TOKEN"),
        "{log}"
    );
    session.stop().unwrap();
}

/// A team context as `for_project` makes it, with no file read from a project folder: the
/// app-wide instructions saved in its own engine data folder, the owner's text for the one
/// agent and the hooks given. Inputs: the app text, the agent text and the hooks. Output:
/// the data folder's guard and the context.
fn team_context(
    app: &str,
    agent: &str,
    hooks: Vec<EffectiveHook>,
) -> (tempfile::TempDir, AgentContext) {
    let data = tempfile::tempdir().unwrap();
    let project = crate::agent_home::AgentProject {
        data: data.path().canonicalize().unwrap(),
        id: ProjectId::new("listed-team").unwrap(),
        folder: None,
        mode: crate::instruction_files::Mode::Both,
        repeated: crate::agent_home::Repeated::LeaveOut,
    };
    crate::agent_home::save_app(&project.data, app).unwrap();
    let mut context = context();
    context.process = Some(Box::new(Process {
        project,
        hooks,
        agent: agent.into(),
        caches: crate::agent_home::Caches::default(),
    }));
    (data, context)
}

/// T19b prerequisite (fix8 Q12 A): a team agent of a listed harness is given its project's
/// context. The instruction file its manifest names holds the startup instruction first,
/// then the owner's app text and the owner's text for this agent.
#[test]
fn a_team_context_gives_a_listed_harness_the_owners_app_and_agent_text_in_its_instruction_file() {
    let (_folder, home) = private_home();
    let (_data, context) = team_context("OWNER-APP-5c1\n", "AGENT-TEXT-5c2", Vec::new());

    let installed = context.install_listed(&manifest(BLOCKS), &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    let written = fs::read_to_string(home.join("agent").join("AGENTS.md")).unwrap();
    assert!(written.starts_with(INSTRUCTIONS), "{written}");
    let app = written.find("OWNER-APP-5c1");
    let agent = written.find("AGENT-TEXT-5c2");
    assert!(app.is_some() && agent.is_some(), "{written}");
    assert!(app < agent, "{written}");
    drop(installed);
}

/// T19b prerequisite (fix8 Q12 A): a listed harness has no way to run the owner's hooks, so
/// a tools-on team agent whose project turned hooks on is refused in plain words, never
/// started with its hooks dropped; with tools off no hook applies and it starts.
#[test]
fn a_tools_on_team_agent_of_a_listed_harness_is_refused_when_its_project_turned_hooks_on() {
    let (_folder, home) = private_home();
    let hook = EffectiveHook {
        plugin: "guard".into(),
        event: "PreToolUse".into(),
        matcher: None,
        command: "/usr/bin/true".into(),
        timeout: None,
        folder: PathBuf::from("/missing-hook-folder"),
    };
    let (_data, context) = team_context("OWNER-APP-5c3\n", "", vec![hook]);

    let refused = context.install_listed(&manifest(BLOCKS), &home, true);

    assert_eq!(
        refused.err().map(|error| error.0),
        Some("launch_listed_hooks_unsupported".to_owned())
    );
    let tools_off = context.install_listed(&manifest(BLOCKS), &home, false);
    assert!(tools_off.is_ok(), "{:?}", tools_off.as_ref().err());
}

/// T19b prerequisite (fix8 Q12 A): `install` finds a listed harness by the registry, not by
/// a piped driver, so a harness that runs only in a terminal is installed too.
#[test]
fn install_finds_a_terminal_only_listed_harness_by_the_registry() {
    let (_folder, home) = harness_home("standin-screen");
    assert!(crate::harnesses::drivers::piped("standin-screen").is_err());

    let installed = context().install("standin-screen", &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
}

const SAVED_KEY: &str = "sk-saved-key-never-in-the-agent-0123456789";

const KEYLESS_URL: &str = "http://127.0.0.1:11434/v1";

const KEYED_URL: &str = "https://api.example.org/v1";

/// One pool account in the stand-in ACP harness's wire format, as the engine stores it.
/// Inputs: its address and whether a key was saved with it. Output: the connection.
fn openai_chat(url: &str, keyed: bool) -> crate::config::ModelConnection {
    let auth = if keyed {
        serde_json::json!({"method": "api_key", "credential_ref": "row-key"})
    } else {
        serde_json::json!({"method": "none"})
    };
    serde_json::from_value(serde_json::json!({
        "id": "row-account",
        "provider": "openai-compatible",
        "harness": "rcli",
        "protocol": "openai-chat",
        "account_ref": "row-login",
        "endpoint": {"kind": "http", "url": url},
        "model": {"id": "qwen2.5:0.5b", "author": "openai-compatible"},
        "auth": auth,
    }))
    .unwrap()
}

/// The stand-in ACP harness as the test registry lists it. Output: the harness.
fn standin_acp() -> &'static crate::harnesses::registry::Harness {
    crate::harnesses::registry::registry()
        .get("standin-acp")
        .unwrap()
}

/// A working folder and a project profile for one ACP launch row. Output: the guards,
/// the canonical working folder and the profile path.
fn work_and_profile() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let working = work.path().canonicalize().unwrap();
    let profiles = tempfile::tempdir().unwrap();
    let profile = profiles.path().canonicalize().unwrap().join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    (work, profiles, working, profile)
}

/// The stand-in's ACP launch request on one account binding, inside a project profile, or
/// at Full access when none is given. Inputs: the working folder, the profile and the
/// binding. Output: the request.
fn acp_request(
    working: &Path,
    profile: Option<PathBuf>,
    account: crate::harnesses::launch::AccountBinding,
) -> crate::harnesses::launch::LaunchRequest {
    crate::harnesses::launch::LaunchRequest {
        harness: "standin-acp".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: working.to_path_buf(),
        account,
        model: Some("qwen2.5:0.5b".into()),
        tools: true,
        unconfined: profile.is_none(),
        sandbox_profile: profile,
        git_identity: None,
        limits: crate::harnesses::launch::LaunchLimits::default(),
    }
}

/// The variables a built command sets, by name. Input: the command. Output: the map.
fn variables(command: &std::process::Command) -> std::collections::BTreeMap<String, String> {
    command
        .get_envs()
        .filter_map(|(name, value)| {
            Some((
                name.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect()
}

/// T19b 19b-2: an ACP launch on a keyless account is handed its own address and the local
/// placeholder in the variables its manifest's `accounts` entry names, and the engine's
/// HOME still wins.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_on_a_keyless_account_is_handed_its_address_and_the_local_placeholder() {
    let (_folder, home) = harness_home("standin-acp");
    let (_work, _profiles, working, profile) = work_and_profile();
    let handover =
        crate::harnesses::accounts::handover(standin_acp(), &openai_chat(KEYLESS_URL, false), None)
            .unwrap();
    let context = context().with_handover(handover);
    let request = acp_request(
        &working,
        Some(profile),
        crate::harnesses::launch::AccountBinding::Url {
            home: home.clone(),
            base_url: KEYLESS_URL.into(),
            key: None,
        },
    );

    let built = crate::harnesses::launch::command_with_context(&request, &context);

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let (command, _installed) = built.unwrap();
    let set = variables(&command);
    assert_eq!(
        set.get("OPENAI_BASE_URL").map(String::as_str),
        Some(KEYLESS_URL)
    );
    assert_eq!(
        set.get("OPENAI_API_KEY").map(String::as_str),
        Some(crate::model_gateway::relay::LOCAL_PLACEHOLDER)
    );
    assert_eq!(
        set.get("STANDIN_MODEL").map(String::as_str),
        Some("qwen2.5:0.5b")
    );
    assert_eq!(set.get("HOME").map(String::as_str), home.to_str());
}

/// T19b 19b-2: an ACP launch on a keyed account is handed the engine relay's loopback URL
/// and its placeholder token, the token only under the name its manifest lists for it, and
/// the saved key in no argument or variable.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_on_a_keyed_account_is_handed_its_relay_and_token_and_never_the_key() {
    let (_folder, home) = harness_home("standin-acp");
    let (_work, _profiles, working, profile) = work_and_profile();
    let relay = crate::model_gateway::relay::Relay::start(
        KEYED_URL,
        crate::credentials::SecretString::new(SAVED_KEY.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    let handover = crate::harnesses::accounts::handover(
        standin_acp(),
        &openai_chat(KEYED_URL, true),
        Some(&relay),
    )
    .unwrap();
    let context = context().with_handover(handover);
    let request = acp_request(
        &working,
        Some(profile),
        crate::harnesses::launch::AccountBinding::Url {
            home: home.clone(),
            base_url: relay.base_url(),
            key: Some(crate::harnesses::launch::Secret::new(relay.token().to_owned()).unwrap()),
        },
    );

    let built = crate::harnesses::launch::command_with_context(&request, &context);

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let (command, _installed) = built.unwrap();
    let set = variables(&command);
    assert_eq!(
        set.get("OPENAI_BASE_URL").map(String::as_str),
        Some(relay.base_url().as_str())
    );
    let holding: Vec<&str> = set
        .iter()
        .filter(|(_, value)| value.contains(relay.token()))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(holding, ["OPENAI_API_KEY"]);
    let seen = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .chain(
            set.iter()
                .flat_map(|(name, value)| [name.clone(), value.clone()]),
        );
    for text in seen {
        assert!(!text.contains(SAVED_KEY), "{text}");
        assert!(!text.contains(KEYED_URL), "{text}");
    }
}

/// T19b 19b-2: a hand-over made for another address than the launch's binding names is
/// refused, so a launch never tells its harness one account while it is routed to another.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_refuses_a_hand_over_for_another_address_than_its_binding() {
    let (_folder, home) = harness_home("standin-acp");
    let (_work, _profiles, working, profile) = work_and_profile();
    let handover =
        crate::harnesses::accounts::handover(standin_acp(), &openai_chat(KEYLESS_URL, false), None)
            .unwrap();
    let context = context().with_handover(handover);
    let request = acp_request(
        &working,
        Some(profile),
        crate::harnesses::launch::AccountBinding::Url {
            home,
            base_url: "http://127.0.0.1:11435/v1".into(),
            key: None,
        },
    );

    let refused = crate::harnesses::launch::command_with_context(&request, &context);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_account_handover_mismatch".to_owned())
    );
}

/// T19b 19b-2 (ruling t20-q1): the hand-over's one exception to the variable guard exists
/// only under the agent profile, so an ACP launch given a hand-over at Full access is
/// refused, as a listed master's is.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_given_a_hand_over_is_refused_at_full_access() {
    let (_folder, home) = harness_home("standin-acp");
    let (_work, _profiles, working, _profile) = work_and_profile();
    let handover =
        crate::harnesses::accounts::handover(standin_acp(), &openai_chat(KEYLESS_URL, false), None)
            .unwrap();
    let context = context().with_handover(handover);
    let request = acp_request(
        &working,
        None,
        crate::harnesses::launch::AccountBinding::Url {
            home,
            base_url: KEYLESS_URL.into(),
            key: None,
        },
    );

    let refused = crate::harnesses::launch::command_with_context(&request, &context);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_environment_refused".to_owned())
    );
}

/// T19b 19b-2 guard row: Codex and Claude Code read no account hand-over, so an install of
/// either given one is refused before it writes a file.
#[cfg(target_os = "macos")]
#[test]
fn a_native_install_given_a_hand_over_is_refused() {
    let (_folder, home) = harness_home("standin-acp");
    for native in ["codex", "claude-code"] {
        let handover = crate::harnesses::accounts::handover(
            standin_acp(),
            &openai_chat(KEYLESS_URL, false),
            None,
        )
        .unwrap();

        let refused = context()
            .with_handover(handover)
            .install(native, &home, false);

        assert_eq!(
            refused.err().map(|problem| problem.0),
            Some("launch_account_handover_mismatch".to_owned()),
            "{native}"
        );
    }
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
}

/// rv-code4 T19b F2 guard row: a listed install whose context names a tool home its private
/// home is not inside is refused before it writes a file, so a launch never takes its temp
/// folder, `git` or cargo home from a folder its sandbox was not built from; the agent home
/// around the private home is kept on the guard.
#[cfg(target_os = "macos")]
#[test]
fn a_listed_install_keeps_only_a_tool_home_its_private_home_is_inside() {
    let (folder, home) = harness_home("standin-acp");
    let base = folder.path().canonicalize().unwrap();
    let elsewhere = base.join("elsewhere");
    for tool_home in [elsewhere.as_path(), home.as_path()] {
        let refused = context()
            .with_tool_home(tool_home)
            .install("standin-acp", &home, false);

        assert_eq!(
            refused.err().map(|problem| problem.0),
            Some("launch_context_unsafe".to_owned()),
            "{tool_home:?}"
        );
    }
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0);

    let installed = context()
        .with_tool_home(&base)
        .install("standin-acp", &home, false);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    assert_eq!(installed.unwrap().tool_home(), Some(base.as_path()));
}

/// rv-code4 T19b F7: a listed install whose context names a tool home writes the agent's own
/// `getconf` into that home's launch folder, naming the short temp folder the team runtime
/// made there, so the PATH its launch builds from the tool home leads with it and
/// context-mode makes its script folders where the agent may write, as in a native agent.
#[cfg(target_os = "macos")]
#[test]
fn a_listed_install_with_a_tool_home_writes_the_agents_getconf_there() {
    struct TmpRemoved(PathBuf);
    impl Drop for TmpRemoved {
        fn drop(&mut self) {
            let _ = crate::sandbox::toolchains::remove_tmp(&self.0);
        }
    }
    let (folder, home) = harness_home("standin-acp");
    let base = folder.path().canonicalize().unwrap();
    let _removed = TmpRemoved(base.clone());
    let scratch = crate::sandbox::toolchains::make_tmp(&base).unwrap();

    let installed = context()
        .with_tool_home(&base)
        .install("standin-acp", &home, true);

    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());
    let getconf = base.join(STATE).join("bin").join("getconf");
    let script = fs::read_to_string(&getconf).unwrap_or_default();
    assert!(
        script.contains(&scratch.display().to_string()),
        "{getconf:?}: {script}"
    );
    let path = crate::sandbox::toolchains::with_agent_git(&base, "/usr/bin:/bin".to_owned());
    assert!(
        path.starts_with(&format!("{}:", getconf.parent().unwrap().display())),
        "{path}"
    );
    assert!(!home.join(STATE).join("bin").exists());
}

/// T19b 19b-2 guard row: a keyless hand-over given to a launch bound to a relay token at the
/// same address is refused, so the placeholder the harness presents is always the binding's.
#[cfg(target_os = "macos")]
#[test]
fn an_acp_launch_refuses_a_keyless_hand_over_on_a_binding_with_a_relay_token() {
    let (_folder, home) = harness_home("standin-acp");
    let (_work, _profiles, working, profile) = work_and_profile();
    let relay = crate::model_gateway::relay::Relay::start(
        KEYED_URL,
        crate::credentials::SecretString::new(SAVED_KEY.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    let handover =
        crate::harnesses::accounts::handover(standin_acp(), &openai_chat(KEYLESS_URL, false), None)
            .unwrap();
    let context = context().with_handover(handover);
    let request = acp_request(
        &working,
        Some(profile),
        crate::harnesses::launch::AccountBinding::Url {
            home,
            base_url: KEYLESS_URL.into(),
            key: Some(crate::harnesses::launch::Secret::new(relay.token().to_owned()).unwrap()),
        },
    );

    let refused = crate::harnesses::launch::command_with_context(&request, &context);

    assert_eq!(
        refused.err().map(|problem| problem.0),
        Some("launch_account_handover_mismatch".to_owned())
    );
}

/// T19b 19b-4 (rv-secreview note 1): the hooks rule a listed install refuses by is one the
/// research runner can ask before it makes a job's worktree: tools on in a project that
/// turned hooks on is refused; tools off, or no hooks, is not.
#[test]
fn a_context_says_before_a_listed_install_whether_its_hooks_refuse_it() {
    let hook = EffectiveHook {
        plugin: "guard".into(),
        event: "PreToolUse".into(),
        matcher: None,
        command: "/usr/bin/true".into(),
        timeout: None,
        folder: PathBuf::from("/missing-hook-folder"),
    };
    let (_data, hooked) = team_context("OWNER-APP-5c3\n", "", vec![hook]);
    let (_bare, plain) = team_context("OWNER-APP-5c3\n", "", Vec::new());

    let refused = hooked.listed_hooks_refused(true);

    assert!(refused);
    assert!(!hooked.listed_hooks_refused(false));
    assert!(!plain.listed_hooks_refused(true));
    assert!(!context().listed_hooks_refused(true));
}
