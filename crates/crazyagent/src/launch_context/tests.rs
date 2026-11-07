use super::*;
use crate::plugins::{EffectiveSecret, EffectiveServer};
use crate::project_setup::TeamPlan;
use std::time::Duration;

fn stored(folder: &Path) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new("bounds").unwrap(),
        name: "N".repeat(4000),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1,
        revision: 1,
    }
}

#[test]
fn the_startup_instruction_stays_within_its_bounds_whatever_the_project_holds() {
    let folder = tempfile::tempdir().unwrap();
    let context = folder.path().join("context");
    fs::create_dir_all(context.join("documents")).unwrap();
    fs::create_dir_all(context.join("research")).unwrap();
    fs::write(context.join("brief.md"), "é".repeat(20_000)).unwrap();
    for index in 0..200 {
        let name = format!("{index:03}-{}.md", "d".repeat(240));
        fs::write(context.join("documents").join(name), "x").unwrap();
    }
    let memories = format!(
        "## Approved memories\n\n{}",
        "m".repeat(MEMORY_SECTION_BYTES_MAX - 32)
    );
    let setup = stored(folder.path());

    let text = render(&setup.project_id, Some(&setup), &memories, Layout::Linked);

    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX, "{}", text.len());
    assert!(text.contains("The brief continues in context/brief.md."));
    assert!(text.contains("more files in context/"));
    assert!(text.ends_with(&memories));
    assert!(!text.contains(&"N".repeat(NAME_BYTES_MAX + 1)));
}

#[test]
fn a_project_in_a_deeply_nested_folder_is_told_both_its_folders() {
    for length in [900, 1011] {
        let base = tempfile::tempdir().unwrap();
        let mut folder = base.path().canonicalize().unwrap();
        while folder.as_os_str().len() + 100 < length - 50 {
            folder.push("n".repeat(99));
        }
        let rest = length - folder.as_os_str().len() - 1;
        folder.push("m".repeat(rest));
        assert_eq!(folder.as_os_str().len(), length);
        fs::create_dir_all(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        let setup = stored(&folder);

        let text = render(&setup.project_id, Some(&setup), "", Layout::Linked);

        assert!(text.contains(&format!("Project folder: {}\n", folder.display())));
        assert!(text.contains(&format!(
            "Context folder: {}/.crazyagent\n",
            folder.display()
        )));
        assert!(text.len() <= INSTRUCTIONS_BYTES_MAX);
    }
}

#[test]
fn a_project_without_a_setup_still_gets_a_plain_header() {
    let text = render(&ProjectId::new("bare").unwrap(), None, "", Layout::Linked);
    assert_eq!(
        text,
        format!(
            "# Project context from CrazyAgent\n\nYou are working on the project bare.\n{LANDING_RULES}"
        )
    );
    let with_memory = render(
        &ProjectId::new("bare").unwrap(),
        None,
        "## Approved memories\n\nKept.\n",
        Layout::Linked,
    );
    assert!(with_memory.ends_with(&format!(
        "bare.\n{LANDING_RULES}\n## Approved memories\n\nKept.\n"
    )));
}

#[test]
fn clip_never_splits_a_character() {
    assert_eq!(clip("aé", 2), "a");
    assert_eq!(clip("aé", 3), "aé");
    assert_eq!(clip("", 5), "");
}

#[test]
fn the_master_hears_skill_and_server_names_but_no_server_detail() {
    let store = tempfile::tempdir().unwrap();
    let skill = store.path().join("meal-tips");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: meal-tips\ndescription: Gives meal planning tips.\n---\n\nSay tips.\n",
    )
    .unwrap();
    let plugins = EffectivePlugins {
        skills: vec![EffectiveSkill {
            name: "meal-tips".into(),
            folder: skill.clone(),
        }],
        servers: vec![
            EffectiveServer {
                name: "approved-tools".into(),
                spec: ServerSpec::Stdio {
                    command: "/usr/bin/python3".into(),
                    args: vec!["/hidden/server.py".into()],
                },
                secrets: vec![EffectiveSecret {
                    name: "FIXTURE_TOKEN".into(),
                    reference: "secret-reference-1".into(),
                }],
            },
            EffectiveServer {
                name: "remote-docs".into(),
                spec: ServerSpec::Http {
                    url: "https://docs.example.test/mcp".into(),
                },
                secrets: vec![],
            },
        ],
    };

    let text = team_index(&plugins, &RoleSkills::default());

    assert!(text.contains("Your own tools are off"));
    assert!(text.contains("- Skill meal-tips: Gives meal planning tips.\n"));
    assert!(text.contains("- MCP server approved-tools\n"));
    assert!(text.contains("- MCP server remote-docs\n"));
    for hidden in [
        "/usr/bin/python3",
        "server.py",
        "FIXTURE_TOKEN",
        "secret-reference-1",
        "docs.example.test",
        store.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    assert_eq!(
        team_index(&EffectivePlugins::default(), &RoleSkills::default()),
        ""
    );
}

#[test]
fn the_team_index_stays_within_its_bound_with_many_plugins() {
    let plugins = EffectivePlugins {
        skills: (0..300)
            .map(|index| EffectiveSkill {
                name: format!("skill-{index:03}-{}", "s".repeat(40)),
                folder: PathBuf::from("/missing-skill-folder"),
            })
            .collect(),
        servers: (0..100)
            .map(|index| EffectiveServer {
                name: format!("server-{index:03}"),
                spec: ServerSpec::Http {
                    url: "https://example.test/mcp".into(),
                },
                secrets: vec![],
            })
            .collect(),
    };

    let text = team_index(&plugins, &RoleSkills::default());

    assert!(
        text.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK,
        "{}",
        text.len()
    );
    assert!(text.contains("- Skill skill-000-"));
    assert!(!text.contains("- MCP server server-099"));
    assert!(text.ends_with(" more\n"));
}

#[cfg(unix)]
#[test]
fn a_home_is_free_once_its_launch_ends_even_while_a_child_shares_its_lock() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let installed = context.install("codex", &home, false).unwrap();
    let mut child = crate::spawn_gate::spawn(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(installed._lock.try_clone().unwrap())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(
        context.install("codex", &home, false).unwrap_err().0,
        "launch_home_busy"
    );

    drop(installed);
    let again = context.install("codex", &home, false);

    let _ = child.kill();
    let _ = child.wait();
    assert!(
        again.is_ok(),
        "the ended launch still kept its home busy: {:?}",
        again.err()
    );
}

/// A context-less launch's hold takes the same home lock as an install and leaves the home's
/// files as they were (aabe420f): an install or a second hold on the held home is refused, the
/// home keeps its own file and gains only the lock file, and the home is free once the hold
/// ends.
#[cfg(unix)]
#[test]
fn lock_only_holds_the_home_and_leaves_no_files() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    fs::write(home.join("config.toml"), "model = \"kept\"\n").unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };

    let held = lock_only(&home).unwrap();
    assert_eq!(
        context
            .install("codex", &home, false)
            .err()
            .map(|error| error.0),
        Some("launch_home_busy".to_owned()),
        "an install waits for the held home"
    );
    assert_eq!(
        lock_only(&home).err().map(|error| error.0),
        Some("launch_home_busy".to_owned()),
        "a second hold waits for the held home"
    );
    drop(held);

    let mut names: Vec<String> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["config.toml", LOCK]);
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        "model = \"kept\"\n"
    );
    assert!(
        lock_only(&home).is_ok(),
        "the ended hold still kept its home busy"
    );
}

/// RV-DS-74: a second keyed launch on an account home whose agent is already running is
/// refused at the home lock before it writes the key file, so the running agent's relay key
/// file survives. The lock is taken by `command_with_context` ahead of `write_key_file`; the
/// refusal must leave the earlier key file exactly as it was.
#[cfg(unix)]
#[test]
fn a_refused_keyed_launch_on_a_busy_home_keeps_the_running_agents_key_file() {
    use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest, Secret, Session};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let profile = base.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let held = context().install("codex", &home, false).unwrap();
    let key_path = home.join(".crazyagent-key");
    fs::write(&key_path, b"running-agent-relay-token").unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: std::path::PathBuf::from("/usr/bin/true"),
        working_dir: work,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: Some(Secret::new("crazyagent-local".to_owned()).unwrap()),
        },
        model: None,
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let refused = Session::start_with_context(request, context());
    assert_eq!(
        refused.err().map(|error| error.0).as_deref(),
        Some("launch_home_busy")
    );
    assert_eq!(fs::read(&key_path).unwrap(), b"running-agent-relay-token");
    drop(held);
}

#[cfg(unix)]
#[test]
fn a_refused_context_less_durable_launch_on_a_busy_home_keeps_the_running_agents_key_file() {
    use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest, Secret, Session};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let state = base.join("state");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let profile = base.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let held = context().install("codex", &home, false).unwrap();
    let key_path = home.join(".crazyagent-key");
    fs::write(&key_path, b"running-agent-relay-token").unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: std::path::PathBuf::from("/usr/bin/true"),
        working_dir: work,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: Some(Secret::new("crazyagent-local".to_owned()).unwrap()),
        },
        model: None,
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let refused = Session::start_durable(request, None, &state);
    assert_eq!(
        refused.err().map(|error| error.0).as_deref(),
        Some("launch_home_busy")
    );
    assert_eq!(fs::read(&key_path).unwrap(), b"running-agent-relay-token");
    drop(held);
}

#[test]
fn a_team_master_with_tools_on_gets_the_engine_delegate_tool() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let record = root.join("tmp").join(crate::team::tool::DROP_FILE);
    let mailbox = root.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
    let engine = Path::new("/usr/local/bin/crazyagent");
    let fresh = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let context = fresh()
        .with_team_tool(engine, &record, &mailbox, &[])
        .unwrap();
    assert_eq!(
        context.read_paths(),
        [engine.to_path_buf()],
        "the project sandbox lets the agent run the engine executable"
    );
    let home = |name: &str| {
        let home = root.join(name);
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        home
    };
    let codex = home("codex");
    let installed = context.install("codex", &codex, true).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let server = config["mcp_servers"][crate::team::tool::SERVER_NAME]
        .as_table()
        .unwrap();
    assert_eq!(
        server["command"].as_str(),
        Some("/usr/local/bin/crazyagent")
    );
    assert_eq!(
        server["args"],
        toml::Value::Array(vec![
            "team-mcp".into(),
            record.to_str().unwrap().into(),
            mailbox.to_str().unwrap().into()
        ])
    );
    assert!(
        !server.contains_key("env"),
        "the tool server holds no secret"
    );
    drop(installed);
    let installed = context.install("codex", &codex, false).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        !config.contains_key("mcp_servers"),
        "with tools off the master gets no tool"
    );
    drop(installed);

    let claude = home("claude");
    let installed = context.install("claude-code", &claude, true).unwrap();
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude.join(STATE).join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        mcp["mcpServers"][crate::team::tool::SERVER_NAME],
        serde_json::json!({
            "type": "stdio",
            "command": "/usr/local/bin/crazyagent",
            "args": ["team-mcp", record.to_str().unwrap(), mailbox.to_str().unwrap()],
            "env": {},
        })
    );
    drop(installed);

    assert_eq!(
        fresh()
            .with_team_tool(Path::new("crazyagent"), &record, &mailbox, &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        fresh()
            .with_team_tool(engine, Path::new("tmp/record.jsonl"), &mailbox, &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        fresh()
            .with_team_tool(engine, &record, Path::new("tmp/calls"), &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        context
            .with_team_tool(engine, &record, &mailbox, &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_taken"
    );
}

#[test]
fn a_team_master_with_tools_off_hears_the_team_plugin_names() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let skill = root.join("meal-tips");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: meal-tips\ndescription: Gives meal planning tips.\n---\n\nSay tips.\n",
    )
    .unwrap();
    let plugins = EffectivePlugins {
        skills: vec![EffectiveSkill {
            name: "meal-tips".into(),
            folder: skill,
        }],
        servers: vec![],
    };
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    }
    .with_index(&plugins, &RoleSkills::default());
    let home = root.join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();

    let installed = context.install("codex", &home, false).unwrap();

    let config: toml::Table = fs::read_to_string(home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let told = config["developer_instructions"].as_str().unwrap();
    assert!(told.starts_with("# Project context from CrazyAgent\n"));
    assert!(told.contains("Your own tools are off"));
    assert!(told.contains("- Skill meal-tips: Gives meal planning tips.\n"));
    assert!(config.get("mcp_servers").is_none());
    assert!(!home.join("skills").join("crazyagent").exists());
    drop(installed);
}

/// LL2S-1: a skill an agent planted in Codex's own `skills/.system` during one run does not
/// survive the next Codex launch in that home, even when the run ended without its cleanup,
/// so Codex installs its system skills fresh.
#[cfg(unix)]
#[test]
fn a_planted_codex_system_skill_does_not_survive_a_relaunch() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let first = context.install("codex", &home, true).unwrap();
    let planted = home.join("skills").join(".system").join("planted");
    fs::create_dir_all(&planted).unwrap();
    fs::write(
        planted.join("SKILL.md"),
        "---\nname: planted\ndescription: Planted by the agent.\n---\n",
    )
    .unwrap();
    drop(first);
    fs::create_dir_all(&planted).unwrap();
    fs::write(planted.join("SKILL.md"), "planted again\n").unwrap();

    let relaunch = context.install("codex", &home, true).unwrap();

    assert!(
        !home.join("skills").join(".system").exists(),
        "the relaunch leaves the planted system skills for Codex to load"
    );
    drop(relaunch);
}

/// Sets or clears the user immutable flag on one path with the system's `chflags`, as an
/// agent would. Inputs: the flag name and the path. Output: whether `chflags` succeeded.
#[cfg(target_os = "macos")]
fn chflags(flag: &str, path: &Path) -> bool {
    crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/chflags")
            .arg(flag)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .is_ok_and(|done| done.success())
}

/// Clears the user immutable flag on test paths when dropped, so the temporary folder can
/// always be deleted, whatever the test asserted. Holds the paths.
#[cfg(target_os = "macos")]
struct Unlocked(Vec<PathBuf>);

#[cfg(target_os = "macos")]
impl Drop for Unlocked {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = chflags("nouchg", path);
        }
    }
}

/// CTX-M1 r1: a skill an agent planted in Codex's own `skills/.system` and made immutable,
/// or whose `skills` folder it made immutable, does not survive the next Codex launch in
/// that home: the launch clears the flag and removes the folder, so Codex installs its
/// system skills fresh.
#[cfg(target_os = "macos")]
#[test]
fn an_immutable_planted_codex_system_skill_does_not_survive_a_relaunch() {
    for pinned in [".system", "skills"] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap().join("codex");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let context = AgentContext {
            plugins: LaunchPlugins::default(),
            instructions: "# Project context from CrazyAgent\n".into(),
            process: None,
            transcripts: true,
            off_tools: Vec::new(),
            native_team: None,
            relay: None,
            model_proxy: None,
            claude_display: ClaudeDisplay::Classic,
        };
        let first = context.install("codex", &home, true).unwrap();
        drop(first);
        let skills = home.join("skills");
        let system = skills.join(".system");
        let planted = system.join("planted");
        fs::create_dir_all(&planted).unwrap();
        fs::write(planted.join("SKILL.md"), "planted\n").unwrap();
        let flagged = match pinned {
            ".system" => system.clone(),
            _ => skills.clone(),
        };
        let _unlocked = Unlocked(vec![flagged.clone()]);
        assert!(chflags("uchg", &flagged), "{pinned} is made immutable");

        let relaunch = context.install("codex", &home, true);

        assert!(
            !system.exists(),
            "{pinned}: the relaunch leaves the planted system skills for Codex to load"
        );
        drop(relaunch.unwrap());
    }
}

/// CTX-M1 r1: when the planted `skills/.system` cannot be moved away even after its flags
/// are cleared, here because the agent filled the folder the engine moves it to with a
/// file it made immutable, the Codex launch is refused with an ordinary error rather than
/// started with the planted system skills.
#[cfg(target_os = "macos")]
#[test]
fn a_codex_launch_that_cannot_remove_planted_system_skills_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let first = context.install("codex", &home, true).unwrap();
    drop(first);
    let planted = home.join("skills").join(".system").join("planted");
    fs::create_dir_all(&planted).unwrap();
    fs::write(planted.join("SKILL.md"), "planted\n").unwrap();
    let parked = home.join(format!(".{STATE}-system-{}", std::process::id()));
    fs::create_dir(&parked).unwrap();
    let held = parked.join("held");
    fs::write(&held, "held\n").unwrap();
    let _unlocked = Unlocked(vec![held.clone()]);
    assert!(
        chflags("uchg", &held),
        "the parked folder's file is immutable"
    );

    let relaunch = context.install("codex", &home, true);

    assert_eq!(
        relaunch.err().map(|problem| problem.0),
        Some("launch_context_skills_kept".to_owned()),
        "a launch that keeps the planted system skills is refused"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_skills_link_a_sandboxed_agent_plants_never_steers_a_removal_outside_its_home() {
    let agents = crate::sandbox::test_agent::TestAgent::new();
    let marker = agents.peer.join(STATE).join("marker");
    fs::create_dir(agents.peer.join(STATE)).unwrap();
    fs::write(&marker, "peer").unwrap();
    let skills = agents.home.join("skills");
    let plant = format!(
        "rm -rf '{}' && ln -s '{}' '{}'",
        skills.display(),
        agents.peer.display(),
        skills.display()
    );
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: String::new(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };

    agents.shell(&plant);
    assert!(
        fs::symlink_metadata(&skills)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let installed = context.install("codex", &agents.home, false).unwrap();
    assert!(marker.exists(), "launch removed a folder through the link");
    drop(installed);
    assert!(marker.exists(), "cleanup removed a folder through the link");

    assert!(
        !agents
            .run(&format!("rm -f '{}'", skills.display()))
            .status
            .success(),
        "the agent cannot move or remove its skills entry once it is there (LL-2)"
    );
    fs::remove_file(&skills).unwrap();
    let installed = context.install("codex", &agents.home, false).unwrap();
    agents.shell(&plant);
    drop(installed);
    assert!(marker.exists(), "cleanup followed a link planted mid-run");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "peer");

    fs::remove_file(&skills).unwrap();
    fs::create_dir_all(skills.join(STATE).join("old")).unwrap();
    let installed = context.install("codex", &agents.home, false).unwrap();
    assert!(!skills.join(STATE).exists(), "real copies were left behind");
    fs::create_dir(skills.join(STATE)).unwrap();
    drop(installed);
    assert!(!skills.join(STATE).exists(), "real copies were left behind");
    assert!(skills.is_dir() && marker.exists());
}

#[test]
fn a_conversation_master_with_tools_on_is_told_its_tools_and_how_to_start_a_sub_agent() {
    use crate::project_setup::HarnessSlot;
    let folder = tempfile::tempdir().unwrap();
    let mut setup = stored(folder.path());
    setup.team.sub_harnesses = vec![
        HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: true,
        },
        HarnessSlot {
            harness: "claude-code".into(),
            agents: 1,
            tools: false,
        },
    ];

    let text = master_tools_section(&setup, &["codex"]);

    assert!(text.contains("Your tools are on in this conversation"));
    assert!(text.contains("`delegate` tool of the `crazyagent-team` MCP server"));
    assert!(text.contains("Start sub-agents only with this tool"));
    assert!(text.contains(
        "Never use your harness's own sub-agent tools, such as Codex's `spawn_agent`, `followup_task`, `send_message` or `wait_agent`"
    ));
    assert!(text.contains("CrazyAgent stops any agent they start"));
    assert!(!text.contains("sub-agent tools are disabled"));
    assert!(text.contains("appear in the project's Agents tab and Team view"));
    assert!(text.contains("a short note on how it went comes back to this conversation"));
    assert!(!text.contains("not in this conversation"));
    assert!(text.contains("- codex: up to 2 agents, tools on\n"));
    assert!(text.contains("- claude-code: up to 1 agents, tools off\n"));
    assert!(!text.contains("tools are off"));
    assert!(!text.contains("no sub-agents"));

    assert!(!text.contains("any harness"));

    setup.team.sub_harnesses.clear();
    setup.team.max_agents = crate::project_setup::TEAM_AGENTS_MAX;
    let any = master_tools_section(&setup, &["codex", "claude-code"]);
    assert!(any.contains("Your tools are on in this conversation"));
    assert!(!any.contains("no sub-agents"));
    assert!(!any.contains("refuses every task"));
    assert!(
        any.contains("The team plan names no harness, so you may use any harness that can run now:\n- codex\n- claude-code\n"),
        "{any}"
    );
    assert!(any.contains(
        "There is no per-harness limit, only the team maximum of 64 agents, counting you.\n"
    ));
    let waiting = master_tools_section(&setup, &[]);
    assert!(
        waiting.contains("no harness has an account in the project's pool yet"),
        "{waiting}"
    );
    assert!(waiting.contains("only the team maximum of 64 agents"));
    setup.team.max_agents = 1;
    let alone = master_tools_section(&setup, &["codex"]);
    assert!(
        alone.contains(
            "The team maximum is 1 agent, you, so CrazyAgent refuses every task you hand over."
        ),
        "{alone}"
    );
    assert!(!alone.contains("- codex\n"));

    setup.team.sub_harnesses = (0..200)
        .map(|index| HarnessSlot {
            harness: format!("{index}-{}", "h".repeat(250)),
            agents: 1,
            tools: true,
        })
        .collect();
    let crowded = master_tools_section(&setup, &[]);
    assert!(
        crowded.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK,
        "{}",
        crowded.len()
    );
    assert!(crowded.contains(" more\n"));
}

#[test]
fn every_master_and_sub_agent_with_the_team_server_is_told_to_ask_through_ask() {
    use crate::project_setup::HarnessSlot;
    let rule =
        "Ask the owner a question only with the `ask` tool of the `crazyagent-team` MCP server";
    let joint = "Ask the owner a question with the `ask` tool of the `crazyagent-team` MCP server, or as a for me question in `update_work`; both reach the owner. Never ask in your reply text";
    assert_eq!(crate::team::tool::ASK_TOOL, "ask");
    assert_eq!(crate::team::tool::UPDATE_WORK_TOOL, "update_work");
    let folder = tempfile::tempdir().unwrap();
    let mut setup = stored(folder.path());
    let conversation = master_tools_section(&setup, &["codex"]);
    assert_eq!(conversation.matches(rule).count(), 1, "{conversation}");
    assert!(conversation.ends_with(&ask_rule(AskReach::ReplyEnd, false)));
    assert!(conversation.contains("when your reply ends"));
    assert!(conversation.contains(&format!(
        "record it with `{}`",
        crate::team::tool::RECORD_TOOL
    )));
    assert!(conversation.contains("act on it once"));

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let engine = Path::new("/usr/local/bin/crazyagent");
    let record = root.join("tmp").join(crate::team::tool::DROP_FILE);
    let mailbox = root.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
    let fresh = |instructions: String| AgentContext {
        plugins: LaunchPlugins::default(),
        instructions,
        process: None,
        transcripts: false,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let start = "# Project context from CrazyAgent\n";
    assert!(!fresh(start.into()).instructions().contains(rule));

    let drop_file = fresh(start.into())
        .with_team_tool(engine, &record, &mailbox, &[])
        .unwrap();
    assert_eq!(
        drop_file.instructions(),
        format!(
            "{start}{}{}",
            ask_rule(AskReach::ReplyEnd, false),
            memory_tools_section(&[])
        )
    );

    let live = fresh(start.into())
        .with_live_team_tool(engine, &mailbox, &[])
        .unwrap();
    let told = live.instructions();
    assert_eq!(
        told,
        format!(
            "{start}{}{}",
            ask_rule(AskReach::Live, true),
            team_tools_section(&[])
        )
    );
    assert_eq!(told.matches(joint).count(), 1, "{told}");
    assert!(
        !told.contains(rule),
        "the b18 tools section's for me path is never ruled out"
    );
    assert!(told.contains("A for me question reaches the owner only; no agent answers it."));
    assert!(told.contains("one question per call"));
    assert!(told.contains("Put up to 6 choices in `options`"));
    assert!(told.contains("the one you recommend in `recommended`"));
    assert!(told.contains(&format!(
        "record it with `{}`",
        crate::team::tool::RECORD_TOOL
    )));
    assert_eq!(
        live.with_live_team_tool(engine, &mailbox, &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_taken",
        "a second live server is refused before any text is added"
    );

    let unrecorded = [crate::team::tool::RECORD_TOOL.to_owned()];
    let agent = fresh(start.into())
        .with_agent_team_tool(engine, &mailbox, &unrecorded)
        .unwrap();
    let heard = agent.instructions();
    assert_eq!(
        heard,
        format!(
            "{start}{}{}",
            ask_rule(AskReach::SubAgent, true),
            team_tools_section(&unrecorded)
        )
    );
    assert!(heard.contains("types the owner's answer or dismissal into your terminal once"));
    assert_eq!(heard.matches(joint).count(), 1, "{heard}");
    assert!(!heard.contains(rule));
    assert!(!heard.contains(crate::team::tool::RECORD_TOOL));
    let server = &agent.plugins.servers[0];
    assert_eq!(server.name, crate::team::tool::SERVER_NAME);
    assert_eq!(
        server.spec,
        ServerSpec::Stdio {
            command: "/usr/local/bin/crazyagent".into(),
            args: vec![
                crate::team::tool::AGENT_SERVER_COMMAND.into(),
                mailbox.to_str().unwrap().into(),
                crate::team::tool::RECORD_TOOL.into(),
            ],
        }
    );
    assert_eq!(agent.read_paths(), [engine.to_path_buf()]);
    assert_eq!(
        agent
            .with_agent_team_tool(engine, &mailbox, &unrecorded)
            .unwrap_err()
            .0,
        "launch_context_team_tool_taken"
    );
    assert_eq!(
        fresh(start.into())
            .with_agent_team_tool(engine, Path::new("tmp/calls"), &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    let asks_off = fresh(start.into())
        .with_live_team_tool(engine, &mailbox, &[crate::team::tool::ASK_TOOL.to_owned()])
        .unwrap();
    assert!(
        !asks_off.instructions().contains(rule) && !asks_off.instructions().contains(joint),
        "a server with ask off is never told to ask with it"
    );
    let unreported = [crate::team::tool::UPDATE_WORK_TOOL.to_owned()];
    let reports_off = fresh(start.into())
        .with_agent_team_tool(engine, &mailbox, &unreported)
        .unwrap();
    assert_eq!(
        reports_off.instructions(),
        format!(
            "{start}{}{}",
            ask_rule(AskReach::SubAgent, false),
            team_tools_section(&unreported)
        )
    );
    assert_eq!(reports_off.instructions().matches(rule).count(), 1);
    assert!(
        !reports_off.instructions().contains(joint),
        "an agent with update_work off is never told to ask through it"
    );

    for (reach, for_me) in [
        (AskReach::Live, true),
        (AskReach::Live, false),
        (AskReach::ReplyEnd, false),
        (AskReach::SubAgent, true),
        (AskReach::SubAgent, false),
    ] {
        assert!(ask_rule(reach, for_me).len() <= ASK_RULE_BYTES_MAX);
        assert!(!ask_rule(reach, for_me).contains('\u{2014}'));
    }
    let full = fresh("x".repeat(INSTRUCTIONS_BYTES_MAX))
        .with_live_team_tool(engine, &mailbox, &[])
        .unwrap();
    assert!(full.instructions().len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
    assert!(full.instructions().ends_with(&format!(
        "{}{}",
        ask_rule(AskReach::Live, true),
        team_tools_section(&[])
    )));

    setup.team.sub_harnesses = (0..200)
        .map(|index| HarnessSlot {
            harness: format!("{index}-{}", "h".repeat(250)),
            agents: 1,
            tools: true,
        })
        .collect();
    let crowded = master_tools_section(&setup, &[]);
    assert!(crowded.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK);
    assert!(crowded.contains(" more\n"));
    assert!(
        crowded.ends_with(&ask_rule(AskReach::ReplyEnd, false)),
        "a crowded plan drops sub-agent lines, never the rule"
    );
}

#[test]
fn each_launch_writes_the_owner_instructions_into_its_harness_file_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let data = root.join("data");
    let folder = root.join("project");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    crate::agent_home::save_app(&data, "APP-RULE-41\n").unwrap();
    crate::agent_home::save_project(&folder, "PROJECT-RULE-42\n").unwrap();
    let project = crate::agent_home::AgentProject {
        data,
        id: ProjectId::new("process").unwrap(),
        folder: Some(folder),
        mode: crate::instruction_files::Mode::Both,
    };
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: Some(Box::new(Process {
            project,
            hooks: Vec::new(),
            agent: String::new(),
        })),
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let home = |name: &str| {
        let home = root.join(name);
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        home
    };

    let claude = home("claude");
    let installed = context.install("claude-code", &claude, false).unwrap();
    let written = fs::read_to_string(claude.join("CLAUDE.md")).unwrap();
    let app = written.find("APP-RULE-41").unwrap();
    assert!(app < written.find("PROJECT-RULE-42").unwrap());
    let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
    assert!(
        prompt.starts_with("# Project context from CrazyAgent\n") && prompt.ends_with(&written),
        "Claude Code skips its user CLAUDE.md with setting sources off, so its prompt file carries the text"
    );
    assert!(!claude.join("AGENTS.md").exists());
    drop(installed);
    assert_eq!(
        crate::agent_home::received(&claude, crate::agent_home::Harness::Claude)
            .unwrap()
            .unwrap(),
        crate::agent_home::Received {
            text: written.clone(),
        },
        "the owner sees what the last launch received after it ends"
    );

    let codex = home("codex");
    let installed = context.install("codex", &codex, false).unwrap();
    let written = fs::read_to_string(codex.join("AGENTS.md")).unwrap();
    assert!(written.find("APP-RULE-41").unwrap() < written.find("PROJECT-RULE-42").unwrap());
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let developer = config["developer_instructions"].as_str().unwrap();
    assert!(
        !developer.contains("APP-RULE-41"),
        "Codex reads its AGENTS.md itself, so the text is not given twice"
    );
    assert!(!codex.join("CLAUDE.md").exists());
    drop(installed);
}

#[test]
fn every_claude_and_codex_launch_is_told_how_its_work_lands_in_its_layout() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let folder = root.join("project");
    fs::create_dir_all(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    let setup = stored(&folder);
    let linked = [
        "Never create, switch or delete a branch, never commit, push or tag, and never write inside `.git`.",
        "If the sandbox refuses a command, never retry it outside the sandbox or another way",
        "Leave your edits as files in your worktree.",
        "the owner accepts it in the project's Changes view onto the branch checked out in the project folder",
        "This rule wins over any other instruction to branch or commit.",
    ];
    let clone = [
        "Commit your work on the branch checked out in your worktree with conventional commit messages before your task ends: only your commits land, and uncommitted edits stay behind.",
        "Never create, switch, rename or delete a branch, never push, tag or rewrite history, and never write inside the project folder's `.git`.",
        "If the sandbox refuses a command, never retry it outside the sandbox or another way",
        "CrazyAgent fetches your branch, and the owner's Accept in the project's Changes view lands your commits onto the branch checked out in the project folder.",
        "This rule wins over any other instruction about branches or commits.",
    ];
    let home = |name: &str| {
        let home = root.join(name);
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        home
    };

    for (layout, told, never) in [
        (
            Layout::Linked,
            &linked[..],
            &["Commit your work", "only your commits land"][..],
        ),
        (
            Layout::Clone,
            &clone[..],
            &["never commit", "Leave your edits as files"][..],
        ),
    ] {
        let context = AgentContext {
            plugins: LaunchPlugins::default(),
            instructions: render(&setup.project_id, Some(&setup), "", layout),
            process: None,
            transcripts: true,
            off_tools: Vec::new(),
            native_team: None,
            relay: None,
            model_proxy: None,
            claude_display: ClaudeDisplay::Classic,
        };
        let claude = home(&format!("claude-{layout:?}"));
        let installed = context.install("claude-code", &claude, false).unwrap();
        let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
        drop(installed);
        let codex = home(&format!("codex-{layout:?}"));
        let installed = context.install("codex", &codex, false).unwrap();
        let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        let developer = config["developer_instructions"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(installed);

        for (harness, text) in [
            ("claude-code", prompt.as_str()),
            ("codex", developer.as_str()),
        ] {
            assert!(text.contains(landing_rules(layout)), "{harness} {layout:?}");
            for rule in told {
                assert!(text.contains(rule), "{harness} {layout:?} misses: {rule}");
            }
            for rule in never {
                assert!(!text.contains(rule), "{harness} {layout:?} is told: {rule}");
            }
            assert!(
                !text.contains("Never tell a sub-agent"),
                "{harness}: a sub-agent gets the agent rules, not the master's"
            );
        }
    }
    assert_eq!(landing_rules(Layout::Linked), LANDING_RULES);
    assert_eq!(landing_rules(Layout::Clone), OWN_BRANCH_LANDING_RULES);
    assert!(
        render(&ProjectId::new("bare").unwrap(), None, "", Layout::Clone)
            .contains(OWN_BRANCH_LANDING_RULES),
        "a project without a setup is told too"
    );
}

#[test]
fn an_mcp_secret_found_in_the_launch_command_refuses_the_launch_with_a_plain_code() {
    let directory = tempfile::Builder::new()
        .prefix("ca-exposed-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    let work = root.join("work");
    for folder in [&home, &work] {
        fs::DirBuilder::new().mode(0o700).create(folder).unwrap();
    }
    let profile = root.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = || AgentContext {
        plugins: LaunchPlugins {
            skills: Vec::new(),
            servers: vec![LaunchServer {
                name: "tools".into(),
                spec: ServerSpec::Stdio {
                    command: "/usr/bin/true".into(),
                    args: Vec::new(),
                },
                secrets: vec![LaunchSecret {
                    name: "TOKEN".into(),
                    value: crate::plugins::SecretValue::new(
                        home.as_os_str().as_encoded_bytes().to_vec(),
                    ),
                }],
            }],
        },
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let request = || crate::harnesses::launch::LaunchRequest {
        harness: "codex".into(),
        executable: fs::canonicalize("/usr/bin/true").unwrap(),
        working_dir: work.clone(),
        account: crate::harnesses::launch::AccountBinding::Subscription { home: home.clone() },
        model: None,
        tools: true,
        sandbox_profile: Some(profile.clone()),
        git_identity: None,
        limits: crate::harnesses::launch::LaunchLimits::default(),
        unconfined: false,
    };
    assert!(context().exposes_secret(home.as_os_str().as_encoded_bytes()));
    let named = LaunchError(
        "launch_secret_in_command".into(),
        Some(Box::new(crate::harnesses::launch::SecretInCommand {
            server: "tools".into(),
            secret: "TOKEN".into(),
        })),
    );

    let headless = crate::harnesses::launch::command_with_context(&request(), &context());
    let refused = headless.err().unwrap();
    assert_eq!(refused, named);
    let sentence = secret_in_command_text(&refused).unwrap();
    assert_eq!(
        sentence,
        "The MCP server \"tools\" secret \"TOKEN\" is part of the launch command, so it cannot \
         be kept hidden. Replace it in Plugins."
    );
    assert!(!sentence.contains(home.to_str().unwrap()));
    assert_eq!(
        secret_in_command_text(&LaunchError("launch_home_busy".into(), None)),
        None
    );
    let tmux =
        crate::terminal::tmux::installed(&root).expect("tmux must be installed on this machine");
    let host: std::sync::Arc<dyn crate::terminal::Host> = std::sync::Arc::new(
        crate::terminal::tmux::Tmux::open(Some(tmux), &root.join(crate::terminal::FOLDER)),
    );
    let pane = crate::harnesses::terminal_session::TerminalSession::start(
        host,
        crate::terminal::AgentId::team(1).unwrap(),
        request(),
        Some(context()),
    );
    let refused_pane = match std::env::consts::OS {
        "macos" => named,
        "linux" => LaunchError("launch_unavailable_on_linux".into(), None),
        _ => LaunchError("launch_unavailable".into(), None),
    };
    assert_eq!(pane.err(), Some(refused_pane));
    assert!(
        !home.join("config.toml").exists(),
        "a refused launch leaves no generated file holding the secret"
    );
}

/// A Codex agent pane started with an MCP secret in its project context and left running,
/// as an engine leaves it when it quits, with what a later engine needs to adopt it.
#[cfg(target_os = "macos")]
struct LeftPane {
    _directory: tempfile::TempDir,
    config: PathBuf,
    host: std::sync::Arc<dyn crate::terminal::Host>,
    agent: crate::terminal::AgentId,
    record: crate::harnesses::terminal_session::PaneRecord,
    tmux: PathBuf,
    socket: PathBuf,
}

#[cfg(target_os = "macos")]
impl LeftPane {
    /// Starts the pane as team agent `team` in a fresh private home under a `prefix`
    /// folder, then leaves it. Returns the left pane.
    fn start(prefix: &str, team: i64) -> Self {
        let directory = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join("home");
        let work = root.join("work");
        for folder in [&home, &work] {
            fs::DirBuilder::new().mode(0o700).create(folder).unwrap();
        }
        let profile = root.join("project.sb");
        fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        let harness = root.join("harness");
        fs::write(&harness, "#!/bin/sh\nexec sleep 60\n").unwrap();
        fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).unwrap();
        let secret = format!("adopted-secret-{}-{team}", std::process::id());
        let context = AgentContext {
            plugins: LaunchPlugins {
                skills: Vec::new(),
                servers: vec![LaunchServer {
                    name: "tools".into(),
                    spec: ServerSpec::Stdio {
                        command: "/usr/bin/true".into(),
                        args: Vec::new(),
                    },
                    secrets: vec![LaunchSecret {
                        name: "TOKEN".into(),
                        value: crate::plugins::SecretValue::new(secret.as_bytes().to_vec()),
                    }],
                }],
            },
            instructions: "# Project context from CrazyAgent\n".into(),
            process: None,
            transcripts: true,
            off_tools: Vec::new(),
            native_team: None,
            relay: None,
            model_proxy: None,
            claude_display: ClaudeDisplay::Classic,
        };
        let request = crate::harnesses::launch::LaunchRequest {
            harness: "codex".into(),
            executable: harness,
            working_dir: work,
            account: crate::harnesses::launch::AccountBinding::Subscription { home: home.clone() },
            model: None,
            tools: true,
            sandbox_profile: Some(profile),
            git_identity: None,
            limits: crate::harnesses::launch::LaunchLimits::default(),
            unconfined: false,
        };
        let tmux = crate::terminal::tmux::installed(&root)
            .expect("tmux must be installed on this machine");
        let socket = root
            .join(crate::terminal::FOLDER)
            .join(crate::terminal::tmux::SOCKET);
        let host: std::sync::Arc<dyn crate::terminal::Host> =
            std::sync::Arc::new(crate::terminal::tmux::Tmux::open(
                Some(tmux.clone()),
                &root.join(crate::terminal::FOLDER),
            ));
        let agent = crate::terminal::AgentId::team(team).unwrap();
        let config = home.join("config.toml");
        let session = crate::harnesses::terminal_session::TerminalSession::start(
            host.clone(),
            agent.clone(),
            request,
            Some(context),
        )
        .unwrap();
        assert!(fs::read_to_string(&config).unwrap().contains(&secret));
        let record = session.record().cloned().unwrap();
        session.leave();
        assert!(
            config.exists(),
            "leaving keeps the files for the harness that still runs"
        );
        Self {
            _directory: directory,
            config,
            host,
            agent,
            record,
            tmux,
            socket,
        }
    }

    /// Ends the tmux server that holds the pane.
    fn kill_server(&self) {
        let _ = crate::spawn_gate::status(
            std::process::Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        );
    }

    /// Takes the pane over as a later engine does. Returns the session or the refusal.
    fn adopt(&self) -> Result<crate::harnesses::terminal_session::TerminalSession, LaunchError> {
        crate::harnesses::terminal_session::TerminalSession::adopt(
            self.host.clone(),
            self.agent.clone(),
            &self.record,
            crate::harnesses::launch::LaunchLimits::default(),
        )
    }
}

#[cfg(target_os = "macos")]
impl Drop for LeftPane {
    fn drop(&mut self) {
        self.kill_server();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_adopted_pane_removes_the_files_holding_its_mcp_secrets_when_it_stops() {
    let pane = LeftPane::start("ca-adopt-ctx-", 7);
    let adopted = pane.adopt().unwrap();
    drop(adopted);
    assert!(
        !pane.config.exists(),
        "the adopted pane removes the file that holds its MCP secret when it stops"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_left_pane_that_cannot_be_taken_over_has_its_mcp_secret_files_removed() {
    let pane = LeftPane::start("ca-adopt-gone-", 8);
    pane.kill_server();
    assert_eq!(
        pane.adopt().err().map(|refused| refused.0).as_deref(),
        Some("launch_closed")
    );
    assert!(
        !pane.config.exists(),
        "a pane that is gone keeps no file that holds its MCP secret"
    );
}

#[test]
fn the_team_tools_section_lists_each_tool_left_on_with_the_tags_and_the_branch_rule() {
    let all = team_tools_section(&[]);
    for (name, line) in crate::coord::tool_switches::BUILTIN_TOOLS {
        assert_eq!(
            all.matches(&format!("\n- {name}: {line}\n")).count(),
            1,
            "{name}:\n{all}"
        );
    }
    assert!(all.contains("Every question carries two tags: for (me for the owner, master, or lead for your own lead) and severity (high, normal or low)."));
    assert!(all.contains("an agent you lead works on `<your task branch>--<its name>`, never `<your task branch>/<its name>`"));
    assert!(all.len() <= TOOLS_SECTION_BYTES_MAX);
    let off = vec![
        crate::team::tool::SPAWN_TOOL.to_owned(),
        crate::team::tool::STOP_TOOL.to_owned(),
        crate::team::tool::MERGE_TOOL.to_owned(),
    ];
    let some = team_tools_section(&off);
    for name in &off {
        assert!(!some.contains(&format!("\n- {name}: ")), "{name}:\n{some}");
    }
    assert!(some.contains(&format!("\n- {}: ", crate::team::tool::UPDATE_WORK_TOOL)));
    assert!(some.contains("severity (high, normal or low)"));
    assert!(some.len() < all.len());
}

#[test]
fn a_live_team_server_takes_its_mailbox_and_the_tools_left_out_and_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let mailbox = directory.path().canonicalize().unwrap().join("mailbox");
    let engine = Path::new("/usr/local/bin/crazyagent");
    let fresh = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let off = vec![crate::team::tool::STOP_TOOL.to_owned()];
    for (context, subcommand) in [
        (
            fresh().with_live_team_tool(engine, &mailbox, &off).unwrap(),
            crate::team::tool::LIVE_SERVER_COMMAND,
        ),
        (
            fresh()
                .with_agent_team_tool(engine, &mailbox, &off)
                .unwrap(),
            crate::team::tool::AGENT_SERVER_COMMAND,
        ),
    ] {
        let server = context
            .plugins
            .servers
            .iter()
            .find(|server| server.name == crate::team::tool::SERVER_NAME)
            .unwrap();
        let ServerSpec::Stdio { command, args } = &server.spec else {
            panic!("the team server runs over stdio");
        };
        assert_eq!(command, "/usr/local/bin/crazyagent");
        assert_eq!(
            args,
            &vec![
                subcommand.to_owned(),
                mailbox.to_str().unwrap().to_owned(),
                crate::team::tool::STOP_TOOL.to_owned(),
            ]
        );
        assert!(server.secrets.is_empty());
        assert!(
            context.instructions().ends_with(&team_tools_section(&off)),
            "{}",
            context.instructions()
        );
    }
    let unknown = vec!["status".to_owned()];
    assert_eq!(
        fresh()
            .with_live_team_tool(engine, &mailbox, &unknown)
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        fresh()
            .with_agent_team_tool(engine, Path::new("mailbox"), &[])
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
}

/// Builds the context a project launch gets, with no app or project text, the given
/// plugins and switched-off tools, as `for_project` builds it. Inputs: the temp root, the
/// plugins and the switched-off tools by stored server name. Output: the context.
fn context_with(
    root: &Path,
    plugins: LaunchPlugins,
    off_tools: Vec<(String, Vec<String>)>,
) -> AgentContext {
    let data = root.join("data");
    let folder = root.join("project");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    AgentContext {
        plugins,
        instructions: "# Project context from CrazyAgent\n".into(),
        process: Some(Box::new(Process {
            project: crate::agent_home::AgentProject {
                data,
                id: ProjectId::new("launch").unwrap(),
                folder: Some(folder),
                mode: crate::instruction_files::Mode::Both,
            },
            hooks: Vec::new(),
            agent: String::new(),
        })),
        transcripts: true,
        off_tools,
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    }
}

/// Makes one 0700 agent home under the temp root. Inputs: the root and a name.
/// Output: the home.
fn private_home(root: &Path, name: &str) -> PathBuf {
    let home = root.join(name);
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    home
}

/// A command MCP server with no secrets. Input: its stored name. Output: the server.
fn command_server(name: &str) -> LaunchServer {
    LaunchServer {
        name: name.into(),
        spec: ServerSpec::Stdio {
            command: "/usr/bin/true".into(),
            args: Vec::new(),
        },
        secrets: Vec::new(),
    }
}

/// Reads the MCP server names a Codex launch declared in its `config.toml`.
/// Input: the Codex home. Output: the declared names.
fn codex_servers(home: &Path) -> toml::Table {
    let config: toml::Table = fs::read_to_string(home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    config["mcp_servers"].as_table().unwrap().clone()
}

#[test]
fn the_context_mode_server_is_declared_under_its_plugin_name_for_claude_and_codex() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let plugins = || LaunchPlugins {
        skills: Vec::new(),
        servers: vec![command_server("context-mode"), command_server("graph")],
    };

    let claude = private_home(&root, "claude");
    let installed = context_with(&root, plugins(), Vec::new())
        .install("claude-code", &claude, true)
        .unwrap();
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude.join(STATE).join("mcp.json")).unwrap())
            .unwrap();
    let declared = mcp["mcpServers"].as_object().unwrap();
    assert!(declared.contains_key("plugin_context-mode_context-mode"));
    assert!(declared.contains_key("graph"));
    assert!(!declared.contains_key("context-mode"));
    drop(installed);

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, plugins(), Vec::new())
        .install("codex", &codex, true)
        .unwrap();
    let declared = codex_servers(&codex);
    assert!(declared.contains_key("plugin_context-mode_context-mode"));
    assert!(declared.contains_key("graph"));
    assert!(!declared.contains_key("context-mode"));
    drop(installed);
}

#[test]
fn switched_off_tools_reach_claude_as_denied_names_and_codex_as_disabled_tools() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let plugins = || LaunchPlugins {
        skills: Vec::new(),
        servers: vec![command_server("context-mode"), command_server("graph")],
    };
    let off = || {
        vec![
            ("context-mode".to_owned(), vec!["ctx_purge".to_owned()]),
            (
                "graph".to_owned(),
                vec!["write".to_owned(), "delete".to_owned()],
            ),
        ]
    };

    let claude = private_home(&root, "claude");
    let installed = context_with(&root, plugins(), off())
        .install("claude-code", &claude, true)
        .unwrap();
    assert_eq!(
        installed.denied_tools(),
        [
            "mcp__plugin_context-mode_context-mode__ctx_purge",
            "mcp__graph__write",
            "mcp__graph__delete"
        ]
    );
    drop(installed);

    let off_claude = private_home(&root, "claude-off");
    let installed = context_with(&root, plugins(), off())
        .install("claude-code", &off_claude, false)
        .unwrap();
    assert!(
        installed.denied_tools().is_empty(),
        "a tools-off launch declares no server, so it denies none"
    );
    drop(installed);

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, plugins(), off())
        .install("codex", &codex, true)
        .unwrap();
    assert!(installed.denied_tools().is_empty());
    let declared = codex_servers(&codex);
    let disabled = |name: &str| -> Vec<String> {
        declared[name]["disabled_tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool.as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(disabled("plugin_context-mode_context-mode"), ["ctx_purge"]);
    assert_eq!(disabled("graph"), ["write", "delete"]);
    drop(installed);
}

#[test]
fn every_claude_launch_gets_the_message_relay_plugin_and_codex_gets_none() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let engine = root.join("bin").join("crazyagent");
    for tools in [true, false] {
        let claude = private_home(&root, &format!("claude-{tools}"));
        let mailbox = root.join(format!("mailbox-claude-{tools}"));
        let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
            .with_relay(&engine, &mailbox)
            .install("claude-code", &claude, tools)
            .unwrap();
        let relay = claude.join(crate::coord::relay::PLUGIN_FOLDER);
        let arguments: Vec<&OsString> = installed.arguments().iter().collect();
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair[0] == "--plugin-dir" && pair[1] == relay.as_os_str()),
            "{arguments:?}"
        );
        assert!(installed.runs_hooks());
        assert!(relay.join(".claude-plugin").join("plugin.json").is_file());
        assert!(relay.join("hooks").join("hooks.json").is_file());
        drop(installed);
    }

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_relay(&engine, &root.join("mailbox-codex"))
        .install("codex", &codex, true)
        .unwrap();
    assert!(installed.arguments().is_empty());
    assert!(!installed.runs_hooks());
    assert!(!codex.join(crate::coord::relay::PLUGIN_FOLDER).exists());
    drop(installed);
}

#[test]
fn a_native_lead_gives_its_teammates_its_instructions_and_the_lead_graph() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let worktree = root.join("lead-worktree");
    fs::create_dir_all(&worktree).unwrap();
    let tmux = root.join("bin").join("tmux");
    let lead = private_home(&root, "lead");
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_native_team(&tmux, &worktree)
        .install("claude-code", &lead, true)
        .unwrap();
    let team = installed.native().unwrap();
    assert_eq!(team.tmux, tmux);
    assert_eq!(
        team.instructions,
        lead.join(STATE).join(TEAMMATE_INSTRUCTIONS)
    );
    let own = fs::read_to_string(lead.join(STATE).join("instructions.md")).unwrap();
    let mate = fs::read_to_string(&team.instructions).unwrap();
    assert!(mate.starts_with(&own));
    assert!(
        mate.trim_end().ends_with(&format!(
            "run graphify with `--graph {}/graphify-out/graph.json`.",
            worktree.display()
        )),
        "{mate}"
    );
    drop(installed);

    let off = private_home(&root, "lead-tools-off");
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_native_team(&tmux, &worktree)
        .install("claude-code", &off, false)
        .unwrap();
    assert!(installed.native().is_none());
    assert!(!off.join(STATE).join(TEAMMATE_INSTRUCTIONS).exists());
    drop(installed);
}

/// Cuts the "Tools to use" section out of one launch's instructions, up to the next
/// section. Input: the instructions. Output: the section, empty when there is none.
fn tools_section(text: &str) -> &str {
    let Some(start) = text.find("\n## Tools to use\n") else {
        return "";
    };
    let rest = &text[start + 1..];
    let end = rest[1..].find("\n## ").map_or(rest.len(), |at| at + 2);
    &rest[..end]
}

/// DEC-116: every tools-on launch delivers the tool rules as firm rules, not choices left to
/// the agent: Claude Code's own instructions and its native teammates' file, which every
/// master, lead, sub-agent, teammate and research launch is given, and Codex's developer
/// instructions. Each says to query graphify before reading source, to run any output that is
/// not short and fixed through context-mode, and to check the skills and use any that applies.
#[test]
fn every_tools_on_launch_delivers_the_tool_rules_as_firm_rules() {
    const LOOSE: [&str; 6] = [
        "may pass 20 lines",
        "that fits",
        "keep Bash for",
        "keep the shell for",
        "when `graphify-out/graph.json` is in your worktree",
        "look here for a skill",
    ];
    const SHARED: [&str; 3] = [
        "These rules apply to every task;",
        "whose output is not short and fixed through",
        "- graphify: query the project graph before you read or search source. Run `graphify query \"<question>\"` in the shell first,",
    ];
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let folder = root.join("skill-store").join("tdd");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("SKILL.md"),
        "---\nname: tdd\ndescription: Teaches tdd.\n---\n\nSteps.\n",
    )
    .unwrap();
    let context = || {
        context_with(
            &root,
            LaunchPlugins {
                skills: vec![EffectiveSkill {
                    name: "tdd".into(),
                    folder: folder.clone(),
                }],
                servers: vec![command_server("context-mode")],
            },
            Vec::new(),
        )
    };
    let worktree = root.join("lead-worktree");
    fs::create_dir_all(&worktree).unwrap();
    let lead = private_home(&root, "lead");
    let installed = context()
        .with_native_team(&root.join("bin").join("tmux"), &worktree)
        .install("claude-code", &lead, true)
        .unwrap();
    let own = fs::read_to_string(lead.join(STATE).join("instructions.md")).unwrap();
    let mate = fs::read_to_string(&installed.native().unwrap().instructions).unwrap();
    drop(installed);
    let codex = private_home(&root, "codex");
    let installed = context().install("codex", &codex, true).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let developer = config["developer_instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(installed);
    let claude_rules = [
        ". Use Bash only for short fixed output, state changes and edits. These tools load on demand: first run ToolSearch with `select:",
        "- Skills: before each task, check the Skills section below and call the Skill tool for every skill there that applies, by its name there, such as `crazyagent:tdd`.",
        "Check this list before every task and use every skill here that applies to it.",
    ];
    let codex_rules = [
        " MCP server. Use the shell only for short fixed output, state changes and edits.",
        "- Skills: before each task, check the Skills section below, then read the SKILL.md of every skill there that applies and follow it.",
        "Check this list before every task and use every skill here that applies to it.",
    ];
    for (launch, text, rules) in [
        ("claude-code", &own, claude_rules),
        ("native teammate", &mate, claude_rules),
        ("codex", &developer, codex_rules),
    ] {
        for rule in SHARED.iter().chain(rules.iter()) {
            assert!(text.contains(rule), "{launch} lacks {rule:?}: {text}");
        }
        for loose in LOOSE {
            assert!(
                !text.contains(loose),
                "{launch} still says {loose:?}: {text}"
            );
        }
    }
}

/// T3-15: the Task-start reminder names the exact ToolSearch select string the launch's own
/// "Tools to use" section gives Claude Code and the approved skills by their Skill tool
/// names, Codex's context-mode tools on their MCP server and its skills by SKILL.md, and
/// nothing with tools off, for a harness without either, or with nothing to name.
#[test]
fn the_task_start_reminder_names_the_select_string_and_skills_per_harness_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let skill = |name: &str| EffectiveSkill {
        name: name.into(),
        folder: root.join("skill-store").join(name),
    };
    for name in ["brainstorming", "tdd"] {
        fs::create_dir_all(root.join("skill-store").join(name)).unwrap();
        fs::write(
            root.join("skill-store").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Teaches {name}.\n---\n\nSteps.\n"),
        )
        .unwrap();
    }
    let plugins = |skills: Vec<EffectiveSkill>, context_mode: bool| LaunchPlugins {
        skills,
        servers: match context_mode {
            true => vec![command_server("context-mode"), command_server("graph")],
            false => vec![command_server("graph")],
        },
    };
    let off = |tools: &[&str]| {
        vec![(
            "context-mode".to_owned(),
            tools.iter().map(|tool| (*tool).to_owned()).collect(),
        )]
    };
    let both = context_with(
        &root,
        plugins(vec![skill("brainstorming"), skill("tdd")], true),
        off(&["ctx_fetch_and_index"]),
    );
    let select = "select:mcp__plugin_context-mode_context-mode__ctx_batch_execute,mcp__plugin_context-mode_context-mode__ctx_execute,mcp__plugin_context-mode_context-mode__ctx_execute_file,mcp__plugin_context-mode_context-mode__ctx_search";
    assert_eq!(
        both.task_start("claude-code", true).as_deref(),
        Some(
            format!(
                "Task start reminder: before you work on this task, run ToolSearch with `{select}` and run every command, test, search or file read whose output is not short and fixed through those context-mode tools (Bash only for short fixed output, state changes and edits), query the project graph with `graphify query \"<question>\"` before you read or search source, and check these skills and call the Skill tool for each one that applies: `crazyagent:brainstorming`, `crazyagent:tdd`."
            )
            .as_str()
        )
    );
    assert_eq!(
        both.task_start("claude", true),
        both.task_start("claude-code", true)
    );
    let home = private_home(&root, "claude");
    let installed = both.install("claude-code", &home, true).unwrap();
    let prompt = fs::read_to_string(home.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    assert!(
        tools_section(&prompt).contains(&format!("ToolSearch with `{select}`")),
        "the reminder's select string is the one the launch's own section gives: {prompt}"
    );
    assert_eq!(
        both.task_start("codex", true).as_deref(),
        Some(
            "Task start reminder: before you work on this task, run every command, test, search or file read whose output is not short and fixed through the `ctx_batch_execute`, `ctx_execute`, `ctx_execute_file`, `ctx_search` tools of the `plugin_context-mode_context-mode` MCP server (the shell only for short fixed output, state changes and edits), query the project graph with `graphify query \"<question>\"` before you read or search source, and check these skills and read and follow the SKILL.md of each one that applies: `brainstorming`, `tdd`."
        )
    );
    assert_eq!(both.task_start("claude-code", false), None, "tools off");
    assert_eq!(both.task_start("codex", false), None, "tools off");
    assert_eq!(both.task_start("rcli", true), None, "no reminder for rcli");

    let skills_only = context_with(&root, plugins(vec![skill("tdd")], false), Vec::new());
    assert_eq!(
        skills_only.task_start("claude-code", true).as_deref(),
        Some(
            "Task start reminder: before you work on this task, query the project graph with `graphify query \"<question>\"` before you read or search source, and check these skills and call the Skill tool for each one that applies: `crazyagent:tdd`."
        )
    );
    let tools_only = context_with(&root, plugins(Vec::new(), true), Vec::new());
    assert_eq!(
        tools_only.task_start("codex", true).as_deref(),
        Some(
            "Task start reminder: before you work on this task, run every command, test, search or file read whose output is not short and fixed through the `ctx_batch_execute`, `ctx_execute`, `ctx_execute_file`, `ctx_fetch_and_index`, `ctx_search` tools of the `plugin_context-mode_context-mode` MCP server (the shell only for short fixed output, state changes and edits), and query the project graph with `graphify query \"<question>\"` before you read or search source."
        )
    );
    let every_tool_off = context_with(
        &root,
        plugins(Vec::new(), true),
        off(&[
            "ctx_batch_execute",
            "ctx_execute",
            "ctx_execute_file",
            "ctx_fetch_and_index",
            "ctx_search",
        ]),
    );
    assert_eq!(every_tool_off.task_start("claude-code", true), None);
    let nothing = context_with(&root, plugins(Vec::new(), false), Vec::new());
    assert_eq!(nothing.task_start("claude-code", true), None);

    let many: Vec<EffectiveSkill> = (0..200)
        .map(|at| skill(&format!("{at:03}{}", "s".repeat(61))))
        .collect();
    let bounded = context_with(&root, plugins(many, true), Vec::new());
    for harness in ["claude-code", "codex"] {
        let text = bounded.task_start(harness, true).unwrap();
        assert!(
            text.len() <= TASK_START_BYTES_MAX,
            "{harness}: {}",
            text.len()
        );
        assert!(
            text.contains(&format!(
                "`{}000{}`",
                if harness == "codex" {
                    ""
                } else {
                    "crazyagent:"
                },
                "s".repeat(61)
            )),
            "{harness}: {text}"
        );
        assert!(
            text.ends_with(", and the other skills in the Skills section."),
            "{harness}: {text}"
        );
    }
}

#[test]
fn a_tools_on_launch_is_told_when_to_use_context_mode_graphify_and_its_skills() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let source = root.join("skill-store").join("brainstorming");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\nname: brainstorming\ndescription: Explores intent before new work.\n---\n\nSteps.\n",
    )
    .unwrap();
    let plugins = |skills: bool| LaunchPlugins {
        skills: match skills {
            true => vec![EffectiveSkill {
                name: "brainstorming".into(),
                folder: source.clone(),
            }],
            false => Vec::new(),
        },
        servers: vec![command_server("context-mode"), command_server("graph")],
    };
    let off = || {
        vec![(
            "context-mode".to_owned(),
            vec!["ctx_fetch_and_index".to_owned()],
        )]
    };
    let declared = |tool: &str| format!("mcp__plugin_context-mode_context-mode__{tool}");
    let on = [
        "ctx_batch_execute",
        "ctx_execute",
        "ctx_execute_file",
        "ctx_search",
    ];

    let claude = private_home(&root, "claude");
    let context = context_with(&root, plugins(true), off());
    crate::agent_home::save_app(&root.join("data"), "APP-RULE-61\n").unwrap();
    let installed = context.install("claude-code", &claude, true).unwrap();
    let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    let section = tools_section(&prompt);
    assert!(!section.is_empty(), "{prompt}");
    assert!(
        section.contains("The owner's instructions and the sections above come first."),
        "{section}"
    );
    assert!(
        !section.contains("as said here"),
        "the section names tools and never outranks other instructions: {section}"
    );
    for tool in on {
        assert!(
            section.contains(&format!("`{}`", declared(tool))),
            "{tool}: {section}"
        );
    }
    assert!(
        section.contains(&format!(
            "ToolSearch with `select:{}`",
            on.map(declared).join(",")
        )),
        "Claude Code defers MCP tools, so the section says how to load them: {section}"
    );
    assert!(
        !section.contains("ctx_fetch_and_index"),
        "a switched-off tool is never named"
    );
    for command in [
        "`graphify query \"<question>\"`",
        "`graphify path \"<A>\" \"<B>\"`",
        "`graphify explain \"<name>\"`",
    ] {
        assert!(section.contains(command), "{command}: {section}");
    }
    assert!(
        section.contains("Skill tool") && section.contains("`crazyagent:brainstorming`"),
        "the Skill tool is named with the exact skill name the index uses: {section}"
    );
    assert!(
        section.contains("`superpowers:NAME`") && section.contains("`crazyagent:NAME`"),
        "a skill the owner's instructions name under its own plugin maps to its name here"
    );
    let tools = prompt.find("\n## Tools to use\n").unwrap();
    assert!(
        prompt.find("APP-RULE-61").unwrap() < tools,
        "the section follows the owner's instructions"
    );
    assert!(
        tools < prompt.find("\n## Skills\n").unwrap(),
        "the section comes just before the skills it points to"
    );

    let codex = private_home(&root, "codex");
    let installed = context_with(&root, plugins(true), off())
        .install("codex", &codex, true)
        .unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let developer = config["developer_instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(installed);
    let section = tools_section(&developer);
    assert!(!section.is_empty(), "{developer}");
    assert!(
        section.contains("The owner's instructions and the sections above come first."),
        "{section}"
    );
    assert!(
        !section.contains("as said here"),
        "the section names tools and never outranks other instructions: {section}"
    );
    for tool in on {
        assert!(section.contains(&format!("`{tool}`")), "{tool}: {section}");
    }
    assert!(
        section.contains("`plugin_context-mode_context-mode`"),
        "{section}"
    );
    assert!(!section.contains("ToolSearch"), "{section}");
    assert!(!section.contains("Skill tool"), "{section}");
    assert!(
        section.contains("`graphify query \"<question>\"`"),
        "{section}"
    );
    assert!(section.contains("SKILL.md"), "{section}");

    let plain = private_home(&root, "claude-plain");
    let installed = context_with(
        &root,
        LaunchPlugins {
            skills: Vec::new(),
            servers: vec![command_server("graph")],
        },
        Vec::new(),
    )
    .install("claude-code", &plain, true)
    .unwrap();
    let prompt = fs::read_to_string(plain.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    let section = tools_section(&prompt);
    assert!(
        section.contains("`graphify query \"<question>\"`"),
        "{prompt}"
    );
    assert!(
        !section.contains("context-mode"),
        "a launch without context-mode is told only what it has: {section}"
    );
    assert!(
        !section.contains("Skill"),
        "a launch without skills is told only what it has: {section}"
    );

    let off_home = private_home(&root, "claude-tools-off");
    let installed = context_with(&root, plugins(true), off())
        .install("claude-code", &off_home, false)
        .unwrap();
    let prompt = fs::read_to_string(off_home.join(STATE).join("instructions.md")).unwrap();
    drop(installed);
    assert!(
        !prompt.contains("## Tools to use"),
        "a tools-off launch has none of these tools"
    );
}

#[test]
fn the_skills_index_gives_each_skill_its_description_and_path_and_says_check_it_first() {
    let sources = tempfile::tempdir().unwrap();
    let folder = Path::new("/private/tmp/home/crazyagent/plugin/skills");
    let long = format!("{}tail", "word ".repeat(60));
    let skills: Vec<EffectiveSkill> = (0..400)
        .map(|index| {
            let name = format!("skill-{index:03}");
            let source = sources.path().join(&name);
            fs::create_dir_all(&source).unwrap();
            let description = match index {
                1 => long.clone(),
                _ => format!("Does task {index}."),
            };
            if index != 2 {
                fs::write(
                    source.join("SKILL.md"),
                    format!("---\nname: {name}\ndescription: {description}\n---\n\nSteps.\n"),
                )
                .unwrap();
            }
            EffectiveSkill {
                name,
                folder: source,
            }
        })
        .collect();
    let index = skills_index(folder, &skills[..3], true);
    assert_eq!(
        index,
        format!(
            "\n## Skills\n\nThe owner approved these skills for this project. Check this list before every task and use every skill here that applies to it. Use one with the Skill tool, where it is named crazyagent:NAME; without a Skill tool, read its SKILL.md at the path shown.\n\n- skill-000: Does task 0. (/private/tmp/home/crazyagent/plugin/skills/skill-000/SKILL.md)\n- skill-001: {}... (/private/tmp/home/crazyagent/plugin/skills/skill-001/SKILL.md)\n- skill-002 (/private/tmp/home/crazyagent/plugin/skills/skill-002/SKILL.md)\n",
            ["word"; 31].join(" ")
        ),
        "a long description is cut at a word within its bound, and a skill whose description cannot be read keeps its name and path"
    );
    assert_eq!(
        skills_index(folder, &skills[..1], false),
        "\n## Skills\n\nThe owner approved these skills for this project. Check this list before every task and use every skill here that applies to it. To use one, read its SKILL.md at the path shown and follow it.\n\n- skill-000: Does task 0. (/private/tmp/home/crazyagent/plugin/skills/skill-000/SKILL.md)\n",
        "a harness with no Skill tool is told to read the file"
    );
    assert_eq!(short_description("one two", 6), "one...");
    assert_eq!(short_description("one twothree", 9), "one...");
    assert_eq!(short_description("abcdefgh", 6), "abc...");
    assert_eq!(short_description("aé bc", 5), "a...");
    let index = skills_index(folder, &skills, true);
    assert!(index.len() <= SKILLS_INDEX_BYTES_MAX + SECTION_SLACK);
    let listed: Vec<&str> = index
        .lines()
        .filter(|line| line.starts_with("- skill-"))
        .collect();
    assert!(listed.len() > 50 && listed.len() < skills.len());
    assert!(listed.iter().all(|line| line.ends_with("/SKILL.md)")));
    assert!(index.ends_with(&format!(
        "- and {} more skills in the same folder\n",
        skills.len() - listed.len()
    )));
}

#[test]
fn a_claude_launch_with_tools_on_writes_only_the_hooks_given_each_rerooted_in_its_own_copy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let data = root.join("data");
    let folder = root.join("project");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    crate::agent_home::save_app(&data, "APP-RULE-51\n").unwrap();
    crate::agent_home::save_project(&folder, "PROJECT-RULE-52\n").unwrap();
    let stored = root.join("hook-store").join("hk-caveman");
    fs::create_dir_all(stored.join("hooks")).unwrap();
    let script = "printf 'root=%s\\n' \"$CLAUDE_PLUGIN_ROOT\"\n";
    fs::write(stored.join("hooks").join("start.sh"), script).unwrap();
    let hook = |event: &str, matcher: Option<&str>, timeout: Option<u32>| EffectiveHook {
        plugin: "caveman".into(),
        event: event.into(),
        matcher: matcher.map(str::to_owned),
        command: "bash \"${CLAUDE_PLUGIN_ROOT}/hooks/start.sh\"".into(),
        timeout,
        folder: stored.clone(),
    };
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: Some(Box::new(Process {
            project: crate::agent_home::AgentProject {
                data,
                id: ProjectId::new("hooked").unwrap(),
                folder: Some(folder),
                mode: crate::instruction_files::Mode::Both,
            },
            hooks: vec![
                hook("SessionStart", None, None),
                hook("PreToolUse", Some("Grep|Glob"), Some(5)),
            ],
            agent: String::new(),
        })),
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    };
    let home = |name: &str| {
        let home = root.join(name);
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        home
    };

    let claude = home("claude");
    let installed = context.install("claude-code", &claude, true).unwrap();
    assert!(installed.runs_hooks());
    let plugin = claude.join(STATE).join("plugin");
    assert!(
        installed
            .arguments()
            .windows(2)
            .any(|pair| pair[0] == "--plugin-dir" && pair[1] == plugin.as_os_str())
    );
    let copy = plugin.join(HOOK_PLUGINS).join("hk-caveman");
    assert_eq!(
        fs::read_to_string(copy.join("hooks").join("start.sh")).unwrap(),
        script
    );
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(plugin.join("hooks").join("hooks.json")).unwrap())
            .unwrap();
    let events = written["hooks"].as_object().unwrap();
    assert_eq!(
        events.keys().map(String::as_str).collect::<Vec<_>>(),
        ["PreToolUse", "SessionStart"]
    );
    let start = &events["SessionStart"][0];
    assert!(start.get("matcher").is_none());
    let command = start["hooks"][0]["command"].as_str().unwrap();
    assert!(!command.contains(stored.to_str().unwrap()), "{command}");
    let ran = crate::spawn_gate::output(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("CLAUDE_PLUGIN_ROOT", &plugin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(
        String::from_utf8(ran.stdout).unwrap(),
        format!("root={}\n", copy.display()),
        "the hook runs from its own copy with the plugin root moved there"
    );
    let search = &events["PreToolUse"][0];
    assert_eq!(search["matcher"], "Grep|Glob");
    assert_eq!(search["hooks"][0]["timeout"], 5);
    let prompt = fs::read_to_string(claude.join(STATE).join("instructions.md")).unwrap();
    let app = prompt.find("APP-RULE-51").unwrap();
    assert!(prompt.starts_with("# Project context from CrazyAgent\n"));
    assert!(app < prompt.find("PROJECT-RULE-52").unwrap());
    drop(installed);

    let off = home("claude-off");
    let installed = context.install("claude-code", &off, false).unwrap();
    assert!(!installed.runs_hooks(), "tools off gives no hooks");
    assert!(!off.join(STATE).join("plugin").exists());
    drop(installed);

    let codex = home("codex");
    let installed = context.install("codex", &codex, true).unwrap();
    assert!(!installed.runs_hooks(), "Codex launches get no hooks");
    assert!(!codex.join(STATE).join("plugin").exists());
    drop(installed);
}

const CONTEXT_MODE_VERSION: &str = "1.0.169";

/// Makes a private, canonical folder for one engine root, agent home or project.
/// Input: the path. Output: the canonical path.
fn private_folder(path: &Path) -> PathBuf {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
    path.canonicalize().unwrap()
}

/// Stores the default pack's context-mode server, `<bin>/npx -y context-mode@<version>`,
/// approved for every project, in a real engine database at `root`, the way the setup pack
/// saves it. Inputs: the engine root and the folder holding npx. Output: the open database.
fn store_context_mode(root: &Path, bin: &Path) -> rusqlite::Connection {
    use crate::storage::plugins::{Command, ResultValue, execute};
    let mut connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
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
    let spec = ServerSpec::Stdio {
        command: bin.join("npx").to_str().unwrap().to_owned(),
        args: vec!["-y".into(), format!("context-mode@{CONTEXT_MODE_VERSION}")],
    };
    let ResultValue::Server(view) = execute(
        &mut connection,
        root,
        Command::AddServer {
            scope: crate::plugins::Scope::Global,
            name: "context-mode".into(),
            spec: spec.clone(),
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    execute(
        &mut connection,
        root,
        Command::Approve { id: view.id, spec },
    )
    .unwrap();
    connection
}

/// Reads the launch set the engine gives the project `app`. Inputs: the database and the
/// engine root. Output: the launch set.
fn launch_app(connection: &mut rusqlite::Connection, root: &Path) -> LaunchPlugins {
    use crate::storage::plugins::{Command, ResultValue, execute};
    let ResultValue::Launch(launch) = execute(
        connection,
        root,
        Command::Launch {
            project: ProjectId::new("app").unwrap(),
        },
    )
    .unwrap() else {
        panic!("launch set required");
    };
    *launch
}

fn context_mode_context(plugins: LaunchPlugins) -> AgentContext {
    AgentContext {
        plugins,
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
        off_tools: Vec::new(),
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
    }
}

/// Reads how one fresh Claude Code home's generated MCP config starts the context-mode
/// server. Inputs: the context and the home. Output: the command and its arguments.
fn claude_context_mode(context: &AgentContext, home: &Path) -> (String, Vec<String>) {
    let installed = context.install("claude-code", home, true).unwrap();
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.join(STATE).join("mcp.json")).unwrap())
            .unwrap();
    drop(installed);
    let server = &mcp["mcpServers"]["plugin_context-mode_context-mode"];
    assert_eq!(server["type"], "stdio");
    let args = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    (server["command"].as_str().unwrap().to_owned(), args)
}

#[test]
fn a_fresh_agent_home_starts_context_mode_from_the_engine_install_not_npx() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = private_folder(&base.join("engine"));
    let bin = private_folder(&base.join("bin"));
    for program in ["npx", "node"] {
        fs::write(bin.join(program), "").unwrap();
        fs::set_permissions(bin.join(program), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut connection = store_context_mode(&root, &bin);
    let waiting = context_mode_context(launch_app(&mut connection, &root));
    let (command, _) = claude_context_mode(&waiting, &private_folder(&base.join("first")));
    assert_eq!(
        command,
        bin.join("npx").to_str().unwrap(),
        "until the engine install is complete an agent keeps the approved npx launch"
    );

    let install = root
        .join("setup")
        .join("npm")
        .join(format!("context-mode@{CONTEXT_MODE_VERSION}"));
    let package = install.join("node_modules").join("context-mode");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"context-mode\",\"version\":\"{CONTEXT_MODE_VERSION}\"}}"),
    )
    .unwrap();
    fs::write(package.join("cli.bundle.mjs"), "").unwrap();
    fs::write(
        install.join("node_modules").join(".package-lock.json"),
        serde_json::json!({"packages": {"node_modules/context-mode": {
            "version": CONTEXT_MODE_VERSION,
            "integrity": "sha512-94JIaFuLjF9SO2BsGTrbGtyT44K95+9OC8BdbaL/UT76xOkanJLfUR5CzmNw+GELXZQqH4nBrKg9wjBnSFkVnQ==",
        }}})
        .to_string(),
    )
    .unwrap();
    let bundle = package.join("cli.bundle.mjs");
    let node = bin.join("node");

    let context = context_mode_context(launch_app(&mut connection, &root));
    let (command, args) = claude_context_mode(&context, &private_folder(&base.join("claude")));
    assert_eq!(
        (command.as_str(), args),
        (
            node.to_str().unwrap(),
            vec![bundle.to_str().unwrap().to_owned()]
        ),
        "a fresh home runs the engine's one install with node, never npx with its own cold cache"
    );

    let codex = private_folder(&base.join("codex"));
    let installed = context.install("codex", &codex, true).unwrap();
    let config: toml::Table = fs::read_to_string(codex.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    drop(installed);
    let server = config["mcp_servers"]["plugin_context-mode_context-mode"]
        .as_table()
        .unwrap();
    assert_eq!(server["command"].as_str(), node.to_str());
    assert_eq!(
        server["args"],
        toml::Value::Array(vec![bundle.to_str().unwrap().into()])
    );

    let reads = context.read_paths();
    assert!(reads.contains(&node), "{reads:?}");
    assert!(
        reads
            .iter()
            .any(|read| read.parent() == Some(install.as_path())),
        "the sandbox lets the agent read the whole install, its dependencies included: {reads:?}"
    );
}

/// Starts one MCP server under the agent sandbox and completes the MCP initialize handshake,
/// a tool listing and then each given tool call in turn, quoting the end of the server's
/// stderr when it stops answering. Inputs: the sandboxed command, with its program and
/// arguments, the `tools/call` params to send and the time allowed for all. Output: the
/// seconds the handshake took, the tool names and each call's reply.
fn mcp_handshake(
    mut command: std::process::Command,
    calls: &[serde_json::Value],
    limit: Duration,
) -> (Duration, Vec<String>, Vec<serde_json::Value>) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    const STDERR_TAIL_BYTES: usize = 4096;
    let errors = tempfile::NamedTempFile::new().unwrap();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(errors.reopen().unwrap());
    let started = std::time::Instant::now();
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, replies) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let reply = |stdin: &mut std::process::ChildStdin, request: serde_json::Value| {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        loop {
            let left = limit.saturating_sub(started.elapsed());
            let line = replies.recv_timeout(left).unwrap_or_else(|_| {
                let text = fs::read(errors.path()).unwrap_or_default();
                let tail = &text[text.len().saturating_sub(STDERR_TAIL_BYTES)..];
                panic!(
                    "the context-mode server answers within {limit:?}; its stderr ends: {}",
                    String::from_utf8_lossy(tail)
                )
            });
            let answer = serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap();
            if answer["id"] == request["id"] {
                break answer;
            }
        }
    };
    let hello = reply(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "crazyagent-test", "version": "0"},
            },
        }),
    );
    let took = started.elapsed();
    assert_eq!(
        hello["result"]["serverInfo"]["name"], "context-mode",
        "{hello}"
    );
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    let listed = reply(
        &mut stdin,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let mut called = Vec::with_capacity(calls.len());
    for (at, params) in calls.iter().enumerate() {
        called.push(reply(
            &mut stdin,
            serde_json::json!({"jsonrpc": "2.0", "id": 3 + at, "method": "tools/call", "params": params}),
        ));
    }
    let _ = child.kill();
    let _ = child.wait();
    let tools = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    (took, tools, called)
}

/// Finds a program on the test's own PATH without following links, as the setup pack's
/// lookup does. Input: the name. Output: the path, if any.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|folder| folder.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// The engine's shared context-mode install under a test folder: an engine root whose
/// `setup/npm` holds context-mode copied from the npm prefix `CRAZYAGENT_CONTEXT_MODE_INSTALL`
/// names, approved for the project `app`. Input: the test folder. Output: the project's
/// launch context, the engine's database and the shared install folder, or a panic naming
/// what is unavailable.
#[cfg(target_os = "macos")]
fn shared_context_mode(base: &Path) -> (AgentContext, rusqlite::Connection, PathBuf) {
    let Some(source) = std::env::var_os("CRAZYAGENT_CONTEXT_MODE_INSTALL") else {
        panic!(
            "UNAVAILABLE: CRAZYAGENT_CONTEXT_MODE_INSTALL, an npm prefix holding node_modules/context-mode@{CONTEXT_MODE_VERSION}"
        );
    };
    let Some(npx) = on_path("npx") else {
        panic!("UNAVAILABLE: npx and node on PATH");
    };
    let bin = npx.parent().unwrap().to_path_buf();
    let root = private_folder(&base.join("engine"));
    let npm = private_folder(&root.join("setup").join("npm"));
    let install = npm.join(format!("context-mode@{CONTEXT_MODE_VERSION}"));
    let copied = crate::spawn_gate::status(
        std::process::Command::new("/bin/cp")
            .arg("-cR")
            .arg(&source)
            .arg(&install),
    )
    .unwrap();
    assert!(copied.success());
    let mut connection = store_context_mode(&root, &bin);
    let context = context_mode_context(launch_app(&mut connection, &root));
    (context, connection, install)
}

#[cfg(target_os = "macos")]
#[test]
fn the_engine_install_of_context_mode_answers_the_mcp_handshake_in_a_fresh_sandboxed_home() {
    use crate::sandbox::toolchains::{ToolHomes, Toolchain, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    const CLAUDE_CONNECT_LIMIT: Duration = Duration::from_secs(30);
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let (context, _connection, install) = shared_context_mode(&base);
    let home = private_folder(&base.join("home"));
    let (command, args) = claude_context_mode(&context, &home);
    assert_ne!(
        Path::new(&command).file_name(),
        Some("npx".as_ref()),
        "a fresh home never starts context-mode through npx"
    );

    let project = private_folder(&base.join("project"));
    let tmp = private_folder(&base.join("tmp"));
    let mut sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
        .unwrap()
        .allow_tools(&Toolchains::grant(
            &ToolHomes::owner().unwrap(),
            &[Toolchain::Node],
        ))
        .unwrap();
    for read in context.read_paths() {
        sandbox = sandbox.allow_read(read.parent().unwrap()).unwrap();
    }
    let profile = base.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let mut server = sandbox.command(&profile, Path::new(&command));
    server.args(&args);
    let (took, tools, _) = mcp_handshake(server, &[], CLAUDE_CONNECT_LIMIT);
    assert!(
        took < CLAUDE_CONNECT_LIMIT,
        "the server answers well inside Claude Code's connect limit: {took:?}"
    );
    assert!(
        tools.iter().any(|tool| tool == "ctx_execute"),
        "the agent gets the context-mode tools: {tools:?}"
    );
    assert!(
        fs::read_dir(&install).unwrap().count() > 0,
        "the shared install stays in place for the next agent"
    );
}

/// T3-15/18: an agent's `ctx_execute` runs. The engine install of context-mode, started from
/// the launch's own config under the deny-default profile the runtime builds for a tools-on
/// agent with its short temp folder, with the harness's variables and the ones Claude Code
/// adds for an MCP server, runs a script whose temp folder is in that short folder, where the
/// launch's own `getconf` points context-mode, not in the per-user temp folder the profile
/// denies.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_runs_ctx_execute_of_the_engine_install_in_its_sandbox() {
    use crate::sandbox::toolchains::{self, ToolHomes, Toolchain, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    const MARKER: &str = "ctx-execute-4471";
    const CALL_LIMIT: Duration = Duration::from_secs(60);
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let (context, _connection, _install) = shared_context_mode(&base);
    let home = private_folder(&base.join("home"));
    let scratch = toolchains::make_tmp(&home).unwrap();
    let installed = context.install("claude-code", &home, true).unwrap();
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.join(STATE).join("mcp.json")).unwrap())
            .unwrap();
    let server = &mcp["mcpServers"]["plugin_context-mode_context-mode"];
    let args: Vec<&str> = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();

    let project = private_folder(&base.join("project"));
    let tmp = private_folder(&base.join("tmp"));
    let mut sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
        .unwrap()
        .allow_tools(&Toolchains::grant(
            &ToolHomes::owner().unwrap(),
            &[Toolchain::Node],
        ))
        .unwrap()
        .allow_scratch(&scratch)
        .unwrap();
    for read in context.read_paths() {
        sandbox = sandbox.allow_read(read.parent().unwrap()).unwrap();
    }
    let profile = base.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let mut harness = std::process::Command::new(server["command"].as_str().unwrap());
    harness.args(&args).current_dir(&project);
    let mut command = sandbox.wrap(&profile, &harness).unwrap();
    command
        .env("CLAUDE_PROJECT_DIR", &project)
        .env("CLAUDECODE", "1");
    for (name, value) in server["env"].as_object().into_iter().flatten() {
        command.env(name, value.as_str().unwrap());
    }
    let call = serde_json::json!({
        "name": "ctx_execute",
        "arguments": {
            "language": "shell",
            "code": format!("printf '%s %s\\n' {MARKER} \"$TMPDIR\""),
        },
    });
    let (_, _, replies) = mcp_handshake(command, &[call], CALL_LIMIT);
    let reply = &replies[0];
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reply["result"]["isError"] != true
            && text.contains(&format!("{MARKER} {}/.ctx-mode-", scratch.display())),
        "ctx_execute runs with its temp folder in the agent's own: {reply}"
    );
    drop(installed);
    toolchains::remove_tmp(&home).unwrap();
}

/// T3-15/18: under the deny-default agent profile with its short temp folder, the agent can
/// neither change nor replace its launch's own `getconf`, add a program beside it or move its
/// folder: the file stays the one the engine wrote.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_cannot_change_its_launch_getconf() {
    use crate::sandbox::toolchains;
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    const CHANGE: &str = r#"printf x 2>/dev/null >> "$1/getconf" && echo changed
printf x 2>/dev/null > "$1/getconf.new" && mv "$1/getconf.new" "$1/getconf" 2>/dev/null && echo replaced
mkdir "$1/planted" 2>/dev/null && echo planted
mv "$1" "$1-moved" 2>/dev/null && echo moved
[ -f "$1/getconf" ] || echo missing
exit 0"#;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = private_folder(&root.join("home"));
    let scratch = toolchains::make_tmp(&home).unwrap();
    let installed = context_with(&root, LaunchPlugins::default(), Vec::new())
        .install("claude-code", &home, true)
        .unwrap();
    let project = private_folder(&root.join("work"));
    let tmp = private_folder(&root.join("tmp"));
    let sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
        .unwrap()
        .allow_scratch(&scratch)
        .unwrap();
    let profile = root.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let bin = home.join(STATE).join("bin");
    let written = fs::read(bin.join("getconf")).ok();
    let mut change = sandbox.command(&profile, Path::new("/bin/sh"));
    change
        .args(["-c", CHANGE, "sh"])
        .arg(&bin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let changed = crate::spawn_gate::output(&mut change).unwrap();
    assert!(changed.status.success(), "{changed:?}");
    assert_eq!(
        String::from_utf8_lossy(&changed.stdout),
        "",
        "the agent's getconf stays the one the engine wrote: {changed:?}"
    );
    assert!(written.is_some());
    assert_eq!(fs::read(bin.join("getconf")).ok(), written);
    drop(installed);
    toolchains::remove_tmp(&home).unwrap();
}

/// T3-15/18: the `getconf` each launch writes into its own folder answers
/// DARWIN_USER_TEMP_DIR with the agent's TMPDIR, ending in a slash as the system one does:
/// the short temp folder for an agent, the lead's own folder in it for a native lead, and the
/// shared one for that lead's teammates, whose HOME is their own home. Any other name goes to
/// the system getconf. The folder leads the harness search path, ahead of the agent's own
/// git, and goes with the launch's files.
#[cfg(target_os = "macos")]
#[test]
fn each_launch_getconf_answers_its_agent_temp_folder_first_on_the_path() {
    use crate::sandbox::toolchains;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let worktree = root.join("lead-worktree");
    fs::create_dir_all(&worktree).unwrap();
    let tmux = root.join("bin").join("tmux");
    let agent = private_home(&root, "agent");
    let lead = private_home(&root, "lead");
    let agent_tmp = toolchains::make_tmp(&agent).unwrap();
    let lead_tmp = toolchains::make_tmp(&lead).unwrap();
    let plain = context_with(&root, LaunchPlugins::default(), Vec::new())
        .install("claude-code", &agent, true)
        .unwrap();
    let native = context_with(&root, LaunchPlugins::default(), Vec::new())
        .with_native_team(&tmux, &worktree)
        .install("claude-code", &lead, true)
        .unwrap();
    let piped = |command: &mut std::process::Command| {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        crate::spawn_gate::output(command).unwrap()
    };
    let answer = |home: &Path, user: &Path, name: &str| {
        let getconf = home.join(STATE).join("bin").join("getconf");
        let metadata = fs::symlink_metadata(&getconf);
        assert!(
            metadata
                .as_ref()
                .is_ok_and(|found| found.is_file() && found.mode() & 0o777 == 0o700),
            "the launch writes its own getconf: {metadata:?}"
        );
        let mut run = std::process::Command::new(&getconf);
        run.arg(name).env_clear().env("HOME", user);
        let output = piped(&mut run);
        assert!(output.status.success(), "{name}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let slashed = |folder: &Path| format!("{}/\n", folder.display());
    assert_eq!(
        answer(&agent, &agent, "DARWIN_USER_TEMP_DIR"),
        slashed(&agent_tmp)
    );
    assert_eq!(
        answer(&lead, &lead, "DARWIN_USER_TEMP_DIR"),
        slashed(&toolchains::native_lead_tmp(&lead_tmp))
    );
    assert_eq!(
        answer(&lead, &mates_home(&lead), "DARWIN_USER_TEMP_DIR"),
        slashed(&lead_tmp)
    );
    let mut system = std::process::Command::new("/usr/bin/getconf");
    system.arg("ARG_MAX");
    let system = piped(&mut system);
    assert_eq!(
        answer(&agent, &agent, "ARG_MAX").into_bytes(),
        system.stdout
    );
    let bin = agent.join(STATE).join("bin");
    let path = toolchains::with_agent_git(&agent, "/usr/bin:/bin".to_owned());
    let entries: Vec<&str> = path.split(':').collect();
    assert_eq!(entries.first().copied(), bin.to_str(), "{path}");
    assert_eq!(entries[entries.len() - 2..], ["/usr/bin", "/bin"], "{path}");
    drop(plain);
    drop(native);
    let path = toolchains::with_agent_git(&agent, "/usr/bin:/bin".to_owned());
    assert!(!path.contains(bin.to_str().unwrap()), "{path}");
    toolchains::remove_tmp(&agent).unwrap();
    toolchains::remove_tmp(&lead).unwrap();
}

#[test]
#[should_panic(expected = "launch_secret_in_command")]
fn secret_names_ride_only_on_a_secret_in_command_refusal() {
    let _ = secret_in_command_text(&LaunchError(
        "launch_home_busy".into(),
        Some(Box::new(SecretInCommand {
            server: "tools".into(),
            secret: "TOKEN".into(),
        })),
    ));
}
/// Starts a local model endpoint that records each request body and answers 500, so a real
/// harness sends its first model request and no reply is made up. Output: the port and the
/// recorded bodies.
#[cfg(target_os = "macos")]
fn request_capture() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    const BODY_BYTES_MAX: usize = 8 * 1024 * 1024;
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = bodies.clone();
    std::thread::spawn(move || {
        for stream in crate::spawn_gate::incoming_tcp_std(&listener).flatten() {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length.min(BODY_BYTES_MAX)];
                if reader.read_exact(&mut body).is_ok() {
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&body).into_owned());
                }
                let mut stream = stream;
                let _ = stream.write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            });
        }
    });
    (port, bodies)
}

/// Installs the Seatbelt profile the team runtime builds for a tools-on agent of a project
/// with the default agent policy: that policy's network beside the model port, its
/// toolchains, the agent's scratch folder, graphify, the harness install folders and the
/// launch context's server folders. Inputs: the test folder, the agent home, the project,
/// the context, the model port and the install folders. Output: the profile path.
#[cfg(target_os = "macos")]
fn tools_on_profile(
    base: &Path,
    home: &Path,
    project: &Path,
    context: &AgentContext,
    port: u16,
    installs: &[&Path],
) -> PathBuf {
    use crate::sandbox::policy::AgentPolicy;
    use crate::sandbox::toolchains::{self, ToolHomes, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxRemote};
    let policy = AgentPolicy::default();
    let homes = ToolHomes::owner().unwrap();
    let tmp = private_folder(&base.join("tmp"));
    let scratch = toolchains::make_tmp(home).unwrap();
    let network = policy.network.sandbox(Some(SandboxRemote::Loopback(port)));
    let (mut sandbox, _) = ProjectSandbox::new(project, home, &tmp, network)
        .unwrap()
        .allow_tools(&Toolchains::grant(&homes, &policy.allowed(project)))
        .unwrap()
        .allow_scratch(&scratch)
        .unwrap()
        .allow_extra_reads(&toolchains::graphify(&homes))
        .unwrap();
    for install in installs {
        sandbox = sandbox.allow_read(install).unwrap();
    }
    for server in context.read_paths() {
        sandbox = sandbox.allow_read(server.parent().unwrap()).unwrap();
    }
    let profile = base.join("agent.sb");
    sandbox.install(&profile).unwrap();
    profile
}

/// The last bytes of a log file, read lossily, or a word saying it is missing. Inputs: the
/// path and the byte bound. Output: the text.
#[cfg(target_os = "macos")]
fn log_tail(path: &Path, bytes: usize) -> String {
    match fs::read(path) {
        Ok(data) => String::from_utf8_lossy(&data[data.len().saturating_sub(bytes)..]).into_owned(),
        Err(_) => "(no log)".to_owned(),
    }
}

/// A fresh Claude Code agent the engine launches with its launch context, context-mode
/// started from the engine's shared install, inside the deny-default Seatbelt profile the
/// runtime builds for a tools-on agent, is offered the context-mode tools: real Claude Code
/// sends its first model request to a local endpoint that records it and answers 500, and
/// that request's tool list names `ctx_execute`, `ctx_batch_execute` and `ctx_search`
/// exactly. Claude Code's stderr goes to a log in the agent home, quoted on failure. Needs
/// Claude Code and `CRAZYAGENT_CONTEXT_MODE_INSTALL`; spends no tokens.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "live: needs Claude Code and CRAZYAGENT_CONTEXT_MODE_INSTALL"]
fn a_fresh_claude_agent_lists_the_context_mode_tools_of_the_engine_install() {
    use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest, Session};
    use std::os::unix::fs::PermissionsExt;
    const TASK: &str = "Name the tools you have for check 4471.";
    const SERVER: &str = "plugin_context-mode_context-mode";
    const STDERR_TAIL_BYTES: usize = 4096;
    let Some(claude) = on_path("claude") else {
        panic!("UNAVAILABLE: no claude executable on PATH");
    };
    let claude = claude.canonicalize().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let (context, _connection, _install) = shared_context_mode(&base);
    let home = private_folder(&base.join("home"));
    let work = private_folder(&base.join("work"));
    let initialized = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .arg(&work),
    )
    .unwrap();
    assert!(initialized.success());
    let stderr = home.join("claude-stderr.log");
    let bin = private_folder(&base.join("bin"));
    let wrapper = bin.join("claude");
    for path in [&claude, &stderr] {
        assert!(!path.to_string_lossy().contains('\''), "{}", path.display());
    }
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' \"$@\" 2>'{}'\n",
            claude.display(),
            stderr.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let (port, bodies) = request_capture();
    let profile = tools_on_profile(
        &base,
        &home,
        &work,
        &context,
        port,
        &[&bin, claude.parent().unwrap()],
    );
    let mut session = Session::start_with_context(
        LaunchRequest {
            harness: "claude-code".into(),
            executable: wrapper,
            working_dir: work,
            account: AccountBinding::Url {
                home,
                base_url: format!("http://127.0.0.1:{port}"),
                key: None,
            },
            model: Some("qwen2.5:0.5b".into()),
            tools: true,
            sandbox_profile: Some(profile),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits {
                run_time: Duration::from_secs(180),
                ..LaunchLimits::default()
            },
        },
        context,
    )
    .unwrap();
    session.send(TASK).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let body = loop {
        if let Some(body) = bodies
            .lock()
            .unwrap()
            .iter()
            .find(|body| body.contains("check 4471") && body.contains("\"tools\""))
        {
            break body.clone();
        }
        let seen = bodies.lock().unwrap().len();
        assert!(
            std::time::Instant::now() < deadline,
            "no model request carried the task with its tools ({seen} requests seen); stderr: {}",
            log_tail(&stderr, STDERR_TAIL_BYTES)
        );
        if session.next_event(Duration::from_millis(500)).is_err() {
            panic!(
                "the agent ended before its first model request ({seen} requests seen); stderr: {}",
                log_tail(&stderr, STDERR_TAIL_BYTES)
            );
        }
    };
    session.stop().unwrap();
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    let offered: Vec<&str> = request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for tool in ["ctx_execute", "ctx_batch_execute", "ctx_search"] {
        let name = format!("mcp__{SERVER}__{tool}");
        assert!(
            offered.contains(&name.as_str()),
            "the agent is offered exactly {name}: {offered:?}"
        );
    }
}
