use super::*;
use crate::plugins::{EffectiveSecret, EffectiveServer};
use crate::project_setup::TeamPlan;

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

    let text = team_index(&plugins);

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
    assert_eq!(team_index(&EffectivePlugins::default()), "");
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

    let text = team_index(&plugins);

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

#[test]
fn a_team_master_with_tools_on_gets_the_engine_delegate_tool() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let record = root.join("tmp").join(crate::team::tool::DROP_FILE);
    let engine = Path::new("/usr/local/bin/crazyagent");
    let fresh = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
    };
    let context = fresh().with_team_tool(engine, &record).unwrap();
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
        toml::Value::Array(vec!["team-mcp".into(), record.to_str().unwrap().into()])
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
            "args": ["team-mcp", record.to_str().unwrap()],
            "env": {},
        })
    );
    drop(installed);

    assert_eq!(
        fresh()
            .with_team_tool(Path::new("crazyagent"), &record)
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        fresh()
            .with_team_tool(engine, Path::new("tmp/record.jsonl"))
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );
    assert_eq!(
        context.with_team_tool(engine, &record).unwrap_err().0,
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
    }
    .with_index(&plugins);
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

    agents.shell(&format!("rm -f '{}'", skills.display()));
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
    assert_eq!(crate::team::tool::ASK_TOOL, "ask");
    let folder = tempfile::tempdir().unwrap();
    let mut setup = stored(folder.path());
    let conversation = master_tools_section(&setup, &["codex"]);
    assert_eq!(conversation.matches(rule).count(), 1, "{conversation}");
    assert!(conversation.ends_with(&ask_rule(AskReach::ReplyEnd)));
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
    };
    let start = "# Project context from CrazyAgent\n";
    assert!(!fresh(start.into()).instructions().contains(rule));

    let drop_file = fresh(start.into()).with_team_tool(engine, &record).unwrap();
    assert_eq!(
        drop_file.instructions(),
        format!("{start}{}", ask_rule(AskReach::ReplyEnd))
    );

    let live = fresh(start.into())
        .with_live_team_tool(engine, &mailbox)
        .unwrap();
    let told = live.instructions();
    assert_eq!(told, format!("{start}{}", ask_rule(AskReach::Live)));
    assert_eq!(told.matches(rule).count(), 1, "{told}");
    assert!(told.contains("never in your reply text"));
    assert!(told.contains("one question per call"));
    assert!(told.contains("Put up to 6 choices in `options`"));
    assert!(told.contains("the one you recommend in `recommended`"));
    assert!(told.contains(&format!(
        "record it with `{}`",
        crate::team::tool::RECORD_TOOL
    )));
    assert_eq!(
        live.with_live_team_tool(engine, &mailbox).unwrap_err().0,
        "launch_context_team_tool_taken",
        "a second live server is refused before any text is added"
    );

    let agent = fresh(start.into())
        .with_agent_team_tool(engine, &mailbox)
        .unwrap();
    let heard = agent.instructions();
    assert_eq!(heard, format!("{start}{}", ask_rule(AskReach::SubAgent)));
    assert!(heard.contains("types the owner's answer or dismissal into your terminal once"));
    assert!(!heard.contains(crate::team::tool::RECORD_TOOL));
    let server = &agent.plugins.servers[0];
    assert_eq!(server.name, crate::team::tool::SERVER_NAME);
    assert_eq!(
        server.spec,
        ServerSpec::Stdio {
            command: "/usr/local/bin/crazyagent".into(),
            args: vec![
                crate::team::tool::questions::AGENT_SERVER_COMMAND.into(),
                mailbox.to_str().unwrap().into(),
            ],
        }
    );
    assert_eq!(agent.read_paths(), [engine.to_path_buf()]);
    assert_eq!(
        agent.with_agent_team_tool(engine, &mailbox).unwrap_err().0,
        "launch_context_team_tool_taken"
    );
    assert_eq!(
        fresh(start.into())
            .with_agent_team_tool(engine, Path::new("tmp/calls"))
            .unwrap_err()
            .0,
        "launch_context_team_tool_invalid"
    );

    for reach in [AskReach::Live, AskReach::ReplyEnd, AskReach::SubAgent] {
        assert!(ask_rule(reach).len() <= ASK_RULE_BYTES_MAX);
        assert!(!ask_rule(reach).contains('\u{2014}'));
    }
    let full = fresh("x".repeat(INSTRUCTIONS_BYTES_MAX))
        .with_live_team_tool(engine, &mailbox)
        .unwrap();
    assert!(full.instructions().len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
    assert!(full.instructions().ends_with(&ask_rule(AskReach::Live)));

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
        crowded.ends_with(&ask_rule(AskReach::ReplyEnd)),
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
        files: crate::instruction_files::Choice::Project,
        owner_home: None,
    };
    let given = crate::agent_home::text(&project).unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: Some(Box::new(Process { project, given })),
        transcripts: true,
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
            skills: Vec::new(),
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
            transcripts: false,
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
