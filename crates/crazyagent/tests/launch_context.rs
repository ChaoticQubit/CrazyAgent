#![cfg(target_os = "macos")]

use crazyagent::agent_git::Layout;
use crazyagent::agent_home;
use crazyagent::client_controller::OwnerController;
use crazyagent::coord::settings::{CoordSettings, RoleSkills, SkillRole};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{AccountSecret, OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchLimits, LaunchRequest, command, command_with_context,
};
use crazyagent::launch_context::{self, AgentContext};
use crazyagent::memory::ReviewDecision;
use crazyagent::memory::library::MemoryScope;
use crazyagent::plugins::{Scope, ServerSpec, ServerStatus};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const APPROVED_SECRET: &str = "dummy-approved-secret-4417";
const WAITING_SECRET: &str = "dummy-waiting-secret-9023";
const HEADER_SECRET: &str = "dummy-header-secret-5150";
const KEPT_MEMORY: &str = "About 2 million households plan meals weekly.";
const PENDING_MEMORY: &str = "Pending insight the owner has not approved.";
const GLOBAL_NOTE: &str = "The owner prefers short, terse answers.";
const OTHER_NOTE: &str = "Only for the other project.";

struct Case {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-lctx-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let home = tempfile::Builder::new()
            .prefix("ca-lctx-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let case = Self {
            root,
            home,
            service: Some(service),
            _stop: stop,
        };
        let mut owner = case.owner().await;
        let picked = case.home.path().canonicalize().unwrap();
        applied(
            owner
                .set_projects_home(picked.to_str().unwrap().into(), 1)
                .await
                .unwrap(),
        );
        case
    }

    async fn owner(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
    }

    fn context(&self, project: &ProjectId, layout: Layout) -> Result<AgentContext, String> {
        let connection = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        launch_context::for_project(&connection, project, layout).map_err(|problem| problem.0)
    }

    fn master(&self, project: &ProjectId, tools: bool) -> Result<(bool, String), String> {
        let connection = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let switch = launch_context::master_tools(&tx, project).map_err(|problem| problem.0)?;
        let text = launch_context::for_master(&tx, project, tools.then_some(&[][..]), None)
            .map_err(|problem| problem.0)?;
        drop(tx);
        Ok((switch, text))
    }

    async fn close(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

fn applied(response: OwnerResponse) {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("owner mutation must commit: {other:?}"),
    }
}

fn skill_folder(root: &Path, name: &str) -> String {
    let path = root.join("picked").join(name);
    fs::create_dir_all(path.join("references")).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Teaches {name}.\n---\n\nSteps for {name}.\n"),
    )
    .unwrap();
    fs::write(path.join("references/notes.md"), format!("{name} notes\n")).unwrap();
    path.display().to_string()
}

fn secret(value: &str) -> AccountSecret {
    AccountSecret::new(value.as_bytes().to_vec()).unwrap()
}

fn private_directory(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

fn request(case: &Case, harness: &str, home: &Path, tools: bool) -> LaunchRequest {
    let profile = case.root.path().join("agent.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    LaunchRequest {
        harness: harness.into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: case.root.path().canonicalize().unwrap(),
        account: AccountBinding::Url {
            home: home.to_path_buf(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            key: None,
        },
        model: Some("qwen2.5:0.5b".into()),
        tools,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(path: &Path) -> BTreeSet<String> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn assert_no_secret_in_command(command: &Command) {
    let mut seen = Vec::new();
    seen.extend(command.get_args().map(OsStr::to_string_lossy));
    for (name, value) in command.get_envs() {
        seen.push(name.to_string_lossy());
        if let Some(value) = value {
            seen.push(value.to_string_lossy());
        }
    }
    for text in seen {
        for secret in [APPROVED_SECRET, WAITING_SECRET, HEADER_SECRET] {
            assert!(
                !text.contains(secret),
                "a secret reached the command line or environment"
            );
        }
    }
}

fn argument_after(command: &Command, flag: &str) -> Option<String> {
    let arguments: Vec<String> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| arguments.get(index + 1).cloned())
}

#[tokio::test]
async fn every_launch_gets_exactly_the_approved_plugins_project_context_and_memories() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let root = case.root.path().to_path_buf();
    let project = ProjectId::new("meals").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Meal planner".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Plans weekly meals for busy families.".into(),
                    tech_stack: vec!["typescript".into()],
                    platforms: vec![Platform::Website],
                    research: vec![ResearchKind::MarketResearch],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap(),
    );
    let other = ProjectId::new("other").unwrap();
    applied(
        owner
            .setup_project(
                other.clone(),
                ProjectSpec {
                    name: "Other".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Unrelated.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap(),
    );
    let folder = owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .state_folder();
    assert_eq!(
        folder,
        case.home
            .path()
            .canonicalize()
            .unwrap()
            .join("meal-planner/.crazyagent")
    );

    owner
        .add_skill(Scope::Global, skill_folder(&root, "release-notes"))
        .await
        .unwrap();
    let switched = owner
        .add_skill(Scope::Global, skill_folder(&root, "switched-off"))
        .await
        .unwrap();
    owner
        .switch_plugin(
            project.clone(),
            switched.id.clone(),
            false,
            switched.version,
        )
        .await
        .unwrap();
    owner
        .add_skill(
            Scope::Project {
                project_id: project.clone(),
            },
            skill_folder(&root, "meal-tips"),
        )
        .await
        .unwrap();
    owner
        .add_skill(
            Scope::Project {
                project_id: other.clone(),
            },
            skill_folder(&root, "other-only"),
        )
        .await
        .unwrap();

    let approved_spec = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec!["--stdio".into(), "two words".into()],
    };
    let approved = owner
        .add_mcp_server(
            Scope::Global,
            "approved-tools".into(),
            approved_spec.clone(),
            vec![("API_TOKEN".into(), secret(APPROVED_SECRET))],
        )
        .await
        .unwrap();
    assert_eq!(
        owner
            .approve_mcp_server(approved.id.clone(), approved_spec.clone())
            .await
            .unwrap()
            .status,
        ServerStatus::Approved
    );
    let remote_spec = ServerSpec::Http {
        url: "https://mcp.example.invalid/mcp".into(),
    };
    let remote = owner
        .add_mcp_server(
            Scope::Project {
                project_id: project.clone(),
            },
            "remote-docs".into(),
            remote_spec.clone(),
            vec![("Authorization".into(), secret(HEADER_SECRET))],
        )
        .await
        .unwrap();
    owner
        .approve_mcp_server(remote.id.clone(), remote_spec.clone())
        .await
        .unwrap();
    owner
        .add_mcp_server(
            Scope::Global,
            "waiting-tools".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/false".into(),
                args: vec![],
            },
            vec![("WAITING_TOKEN".into(), secret(WAITING_SECRET))],
        )
        .await
        .unwrap();

    fs::write(
        folder.join("context/research/market_research.md"),
        format!("# Market\n\n## Size\n{KEPT_MEMORY}\n\n## Trends\n{PENDING_MEMORY}\n"),
    )
    .unwrap();
    applied(
        owner
            .save_research_to_memory(
                project.clone(),
                ResearchKind::MarketResearch,
                Some("Size".into()),
                3,
            )
            .await
            .unwrap(),
    );
    let kept = owner.project_memories(project.clone(), None).await.unwrap();
    assert_eq!(kept.len(), 1);
    applied(
        owner
            .review_memory(
                project.clone(),
                kept[0].id.clone(),
                1,
                ReviewDecision::Keep,
                4,
            )
            .await
            .unwrap(),
    );
    applied(
        owner
            .save_research_to_memory(
                project.clone(),
                ResearchKind::MarketResearch,
                Some("Trends".into()),
                5,
            )
            .await
            .unwrap(),
    );
    applied(
        owner
            .add_memory(MemoryScope::Global, GLOBAL_NOTE.into(), 6)
            .await
            .unwrap(),
    );
    applied(
        owner
            .add_memory(
                MemoryScope::Project {
                    project_id: other.clone(),
                },
                OTHER_NOTE.into(),
                7,
            )
            .await
            .unwrap(),
    );

    let context = case.context(&project, Layout::Linked).unwrap();
    let instructions = context.instructions().to_owned();
    assert!(instructions.len() <= launch_context::INSTRUCTIONS_BYTES_MAX);
    assert!(instructions.contains("Meal planner"));
    assert!(instructions.contains("Plans weekly meals for busy families."));
    assert!(instructions.contains("context/brief.md"));
    assert!(instructions.contains(&format!(
        "Context folder: {}
",
        folder.display()
    )));
    assert!(instructions.contains("context/research/market_research.md"));
    assert!(
        instructions.contains(KEPT_MEMORY),
        "the approved memory is given"
    );
    assert!(
        !instructions.contains(PENDING_MEMORY),
        "a memory waiting for approval is not given"
    );
    assert!(
        instructions.contains(GLOBAL_NOTE),
        "global memory reaches every project"
    );
    assert!(
        !instructions.contains(OTHER_NOTE),
        "another project's memory never reaches this project"
    );
    for secret in [APPROVED_SECRET, WAITING_SECRET, HEADER_SECRET] {
        assert!(!instructions.contains(secret));
        assert!(!format!("{context:?}").contains(secret));
    }

    let (switch, master) = case.master(&project, false).unwrap();
    assert!(
        switch,
        "a new project's team plan turns the master's tools on"
    );
    assert!(master.len() <= launch_context::MASTER_INSTRUCTIONS_BYTES_MAX);
    assert!(
        master.starts_with(&instructions),
        "the master gets the same brief, context index and approved memories"
    );
    for named in [
        "- Skill meal-tips: ",
        "- Skill release-notes: ",
        "- MCP server approved-tools\n",
        "- MCP server remote-docs\n",
    ] {
        assert!(master.contains(named), "{named}");
    }
    for hidden in [
        "waiting-tools",
        "switched-off",
        "other-only",
        "/usr/bin/true",
        "two words",
        "mcp.example.invalid",
        "API_TOKEN",
        APPROVED_SECRET,
        WAITING_SECRET,
        HEADER_SECRET,
        PENDING_MEMORY,
        OTHER_NOTE,
    ] {
        assert!(!master.contains(hidden), "{hidden}");
    }
    let (_, tools_master) = case.master(&project, true).unwrap();
    assert!(tools_master.len() <= launch_context::MASTER_INSTRUCTIONS_BYTES_MAX);
    assert!(tools_master.starts_with(&instructions));
    assert!(tools_master.contains("Your tools are on in this conversation"));
    assert!(tools_master.contains("`delegate` tool of the `crazyagent-team` MCP server"));
    assert!(!tools_master.contains("Your own tools are off"));
    let (_, other_master) = case.master(&other, false).unwrap();
    assert!(other_master.contains("- Skill other-only: "));
    assert!(other_master.contains("- MCP server approved-tools\n"));
    assert!(!other_master.contains("meal-tips"));
    assert!(!other_master.contains("remote-docs"));
    assert!(!other_master.contains(KEPT_MEMORY));
    assert!(other_master.contains(OTHER_NOTE));

    let agents = private_directory(&root.join("agents"));
    let codex_home = private_directory(&agents.join("codex-home"));
    let codex_request = request(&case, "codex", &codex_home, true);
    let (codex_command, codex_files) = command_with_context(&codex_request, &context).unwrap();
    assert_no_secret_in_command(&codex_command);
    assert!(
        !codex_command
            .get_args()
            .any(|argument| argument == "mcp_servers={}"),
        "the generated config, not an empty override, decides the servers"
    );
    let config_path = codex_home.join("config.toml");
    assert_eq!(mode(&config_path), 0o600);
    let config: toml::Table = fs::read_to_string(&config_path).unwrap().parse().unwrap();
    let servers = config["mcp_servers"].as_table().unwrap();
    assert_eq!(
        servers.keys().cloned().collect::<Vec<_>>(),
        ["approved-tools", "remote-docs"],
        "exactly the approved servers, never one waiting for approval"
    );
    let tools = servers["approved-tools"].as_table().unwrap();
    assert_eq!(tools["command"].as_str(), Some("/usr/bin/true"));
    assert_eq!(
        tools["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["--stdio", "two words"]
    );
    assert_eq!(tools["env"]["API_TOKEN"].as_str(), Some(APPROVED_SECRET));
    let docs = servers["remote-docs"].as_table().unwrap();
    assert_eq!(
        docs["url"].as_str(),
        Some("https://mcp.example.invalid/mcp")
    );
    assert_eq!(
        docs["http_headers"]["Authorization"].as_str(),
        Some(HEADER_SECRET)
    );
    assert!(
        config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains(KEPT_MEMORY)
    );
    let codex_skills = codex_home.join("skills/crazyagent");
    assert_eq!(
        names(&codex_skills),
        BTreeSet::from(["meal-tips".to_owned(), "release-notes".to_owned()]),
        "project and global skills, minus the one switched off and other projects' skills"
    );
    assert!(codex_skills.join("release-notes/SKILL.md").is_file());
    assert!(
        codex_skills
            .join("release-notes/references/notes.md")
            .is_file()
    );
    assert_eq!(mode(&codex_skills.join("meal-tips/SKILL.md")), 0o600);

    let claude_home = private_directory(&agents.join("claude-home"));
    let claude_request = request(&case, "claude-code", &claude_home, true);
    let (claude_command, claude_files) = command_with_context(&claude_request, &context).unwrap();
    assert_no_secret_in_command(&claude_command);
    let mcp_path = PathBuf::from(argument_after(&claude_command, "--mcp-config").unwrap());
    assert!(
        mcp_path.starts_with(&claude_home),
        "the MCP config lives in the agent's private state"
    );
    assert_eq!(mode(&mcp_path), 0o600);
    let mcp: serde_json::Value = serde_json::from_slice(&fs::read(&mcp_path).unwrap()).unwrap();
    let declared = mcp["mcpServers"].as_object().unwrap();
    assert_eq!(
        declared.keys().cloned().collect::<Vec<_>>(),
        ["approved-tools", "remote-docs"]
    );
    assert_eq!(declared["approved-tools"]["type"], "stdio");
    assert_eq!(declared["approved-tools"]["command"], "/usr/bin/true");
    assert_eq!(
        declared["approved-tools"]["env"]["API_TOKEN"],
        APPROVED_SECRET
    );
    assert_eq!(declared["remote-docs"]["type"], "http");
    assert_eq!(
        declared["remote-docs"]["headers"]["Authorization"],
        HEADER_SECRET
    );
    assert!(
        claude_command
            .get_args()
            .any(|argument| argument == "--strict-mcp-config")
    );
    let plugin = PathBuf::from(argument_after(&claude_command, "--plugin-dir").unwrap());
    assert!(plugin.join(".claude-plugin/plugin.json").is_file());
    assert_eq!(
        names(&plugin.join("skills")),
        BTreeSet::from(["meal-tips".to_owned(), "release-notes".to_owned()])
    );
    let prompt =
        PathBuf::from(argument_after(&claude_command, "--append-system-prompt-file").unwrap());
    assert_eq!(mode(&prompt), 0o600);
    let prompt_text = fs::read_to_string(&prompt).unwrap();
    assert!(prompt_text.starts_with(&instructions));
    assert!(
        prompt_text.contains(&format!(
            "- meal-tips: Teaches meal-tips. ({})",
            plugin.join("skills/meal-tips/SKILL.md").display()
        )),
        "Claude in bare mode has no Skill tool, so the skills are listed in its instructions"
    );
    assert!(!prompt_text.contains("switched-off"));

    assert_eq!(
        command_with_context(&request(&case, "codex", &codex_home, true), &context)
            .err()
            .map(|problem| problem.0),
        Some("launch_home_busy".into()),
        "two launches never share one agent home"
    );
    drop(codex_files);
    drop(claude_files);
    assert!(
        !config_path.exists(),
        "the secret-bearing config is removed after the launch"
    );
    assert!(!mcp_path.exists());
    assert!(!codex_skills.exists());
    let received = |home: &Path| {
        let received = agent_home::given_skills(case.root.path(), home).unwrap();
        received.into_iter().collect::<BTreeSet<_>>()
    };
    assert_eq!(
        received(&codex_home),
        BTreeSet::from(["meal-tips".to_owned(), "release-notes".to_owned()]),
        "the skill names of the last launch outlive its copies"
    );
    assert_eq!(
        received(&claude_home),
        BTreeSet::from(["meal-tips".to_owned(), "release-notes".to_owned()])
    );

    let off_home = private_directory(&agents.join("off-home"));
    let (off_command, off_files) =
        command_with_context(&request(&case, "codex", &off_home, false), &context).unwrap();
    assert!(
        off_command
            .get_args()
            .any(|argument| argument == "mcp_servers={}")
    );
    let off_config: toml::Table = fs::read_to_string(off_home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        !off_config.contains_key("mcp_servers"),
        "tools off gives no MCP servers"
    );
    assert!(
        off_config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("Meal planner")
    );
    assert!(!off_home.join("skills/crazyagent").exists());
    drop(off_files);
    assert_eq!(
        received(&off_home),
        BTreeSet::new(),
        "tools off installs and records no skill"
    );
    let claude_off = private_directory(&agents.join("claude-off"));
    let (claude_off_command, claude_off_files) =
        command_with_context(&request(&case, "claude-code", &claude_off, false), &context).unwrap();
    let off_mcp: serde_json::Value = serde_json::from_slice(
        &fs::read(argument_after(&claude_off_command, "--mcp-config").unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(off_mcp, serde_json::json!({"mcpServers": {}}));
    assert!(argument_after(&claude_off_command, "--plugin-dir").is_none());
    assert_eq!(
        fs::read_to_string(
            argument_after(&claude_off_command, "--append-system-prompt-file").unwrap()
        )
        .unwrap(),
        instructions,
        "tools off lists no skills"
    );
    drop(claude_off_files);

    let bare = command(&request(&case, "codex", &off_home, true)).unwrap();
    assert!(bare.get_args().any(|argument| argument == "mcp_servers={}"));
    assert!(!off_home.join("config.toml").exists());

    let other_context = case.context(&other, Layout::Linked).unwrap();
    let other_home = private_directory(&agents.join("other-home"));
    let (_, other_files) =
        command_with_context(&request(&case, "codex", &other_home, true), &other_context).unwrap();
    let other_config: toml::Table = fs::read_to_string(other_home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        other_config["mcp_servers"]
            .as_table()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["approved-tools"],
        "another project's servers never reach this project's agents"
    );
    assert_eq!(
        names(&other_home.join("skills/crazyagent")),
        BTreeSet::from([
            "other-only".to_owned(),
            "release-notes".to_owned(),
            "switched-off".to_owned()
        ])
    );
    let other_instructions = other_config["developer_instructions"].as_str().unwrap();
    assert!(!other_instructions.contains(KEPT_MEMORY));
    assert!(other_instructions.contains(GLOBAL_NOTE));
    assert!(other_instructions.contains(OTHER_NOTE));
    drop(other_files);

    assert_eq!(
        case.context(&ProjectId::new("ghost").unwrap(), Layout::Linked)
            .err(),
        Some("plugin_project_unknown".into())
    );
    drop(owner);
    case.close().await;
}

#[tokio::test]
async fn a_codex_agent_with_tools_on_gets_the_skill_index_with_each_description_and_path() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let root = case.root.path().to_path_buf();
    let project = ProjectId::new("notes").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Notes".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Keeps release notes.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap(),
    );
    owner
        .add_skill(Scope::Global, skill_folder(&root, "release-notes"))
        .await
        .unwrap();
    let switched = owner
        .add_skill(Scope::Global, skill_folder(&root, "switched-off"))
        .await
        .unwrap();
    owner
        .switch_plugin(
            project.clone(),
            switched.id.clone(),
            false,
            switched.version,
        )
        .await
        .unwrap();
    let context = case.context(&project, Layout::Linked).unwrap();
    let agents = private_directory(&root.join("agents"));

    let home = private_directory(&agents.join("codex-index"));
    let (_, files) = command_with_context(&request(&case, "codex", &home, true), &context).unwrap();
    let config: toml::Table = fs::read_to_string(home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let developer = config["developer_instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    let skill = home.join("skills/crazyagent/release-notes/SKILL.md");
    assert!(skill.is_file(), "the index names the copy Codex reads");
    assert_eq!(
        developer,
        format!(
            "{}\n## Tools to use\n\nThe owner's instructions and the sections above come first. These rules apply to every task; this section names the tools this launch has as you call them, and where other instructions name one another way, use the name here.\n\n- graphify: query the project graph before you read or search source. Run `graphify query \"<question>\"` in the shell first, `graphify path \"<A>\" \"<B>\"` for how two parts connect and `graphify explain \"<name>\"` for one part. If your worktree has no `graphify-out/graph.json`, say so in your report.\n- Skills: before each task, check the Skills section below, then read the SKILL.md of every skill there that applies and follow it. A skill other instructions name as `superpowers:NAME`, `software-practices:NAME` or `crazyagent:NAME` is the one listed as NAME there.\n\n## Skills\n\nThe owner approved these skills for this project. Check this list before every task and use every skill here that applies to it. To use one, read its SKILL.md at the path shown and follow it.\n\n- release-notes: Teaches release-notes. ({})\n",
            context.instructions(),
            skill.display()
        ),
        "the project context, the Tools to use section of a tools-on launch with no context-mode server (graphify and the Codex skill line), then the index of the one skill on for the project, told to read the file since Codex has no Skill tool"
    );
    drop(files);

    let off = private_directory(&agents.join("codex-off"));
    let (_, off_files) =
        command_with_context(&request(&case, "codex", &off, false), &context).unwrap();
    let off_config: toml::Table = fs::read_to_string(off.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        off_config["developer_instructions"].as_str(),
        Some(context.instructions()),
        "tools off lists no skills"
    );
    drop(off_files);
    drop(owner);
    case.close().await;
}

/// Sets up a project for the team-role skill tests and adds the given global skills from
/// real SKILL.md folders. Inputs: the case, the owner and the skill names. Output: the
/// project.
async fn crew_with_skills(case: &Case, owner: &mut OwnerController, skills: &[&str]) -> ProjectId {
    let project = ProjectId::new("crew").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Crew".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Runs a team.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap(),
    );
    for name in skills {
        owner
            .add_skill(Scope::Global, skill_folder(case.root.path(), name))
            .await
            .unwrap();
    }
    project
}

/// Saves a project's coordination settings through the owner's team action, the path the
/// TUI and the desktop use. Inputs: the owner, the project, the settings and the client
/// generation. Output: the settings the team status then reports, or the refusal code.
async fn save_coordination(
    owner: &mut OwnerController,
    project: &ProjectId,
    settings: CoordSettings,
    generation: u64,
) -> Result<CoordSettings, String> {
    let response = owner
        .team_action(
            crazyagent::team::TeamAction::CoordSettings {
                project_id: project.clone(),
                settings,
            },
            generation,
        )
        .await
        .map_err(|problem| problem.0)?;
    applied(response);
    Ok(coordination(owner, project).await)
}

async fn coordination(owner: &mut OwnerController, project: &ProjectId) -> CoordSettings {
    owner
        .team_status(project.clone())
        .await
        .unwrap()
        .coordination
        .expect("the team status reports the coordination settings")
        .settings
}

/// The skills a Claude Code launch with tools on writes for one team role, from the
/// project's launch context kept to that role's stored setting. Inputs: the case, the
/// project, the role and a private agent home. Output: the written skill folder names.
fn role_launch(case: &Case, project: &ProjectId, role: SkillRole, home: &Path) -> BTreeSet<String> {
    let connection = rusqlite::Connection::open_with_flags(
        case.root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let context =
        launch_context::for_team_role(&connection, project, Layout::Linked, role).unwrap();
    let (command, files) =
        command_with_context(&request(case, "claude-code", home, true), &context).unwrap();
    let written = argument_after(&command, "--plugin-dir")
        .map(|plugin| PathBuf::from(plugin).join("skills"))
        .filter(|skills| skills.is_dir())
        .map_or_else(BTreeSet::new, |skills| names(&skills));
    drop(files);
    written
}

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[tokio::test]
async fn with_all_approved_every_role_gets_every_approved_skill() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let project = crew_with_skills(&case, &mut owner, &["meal-tips", "release-notes"]).await;
    let every = BTreeSet::from_iter(owned(&["meal-tips", "release-notes"]));
    assert_eq!(
        coordination(&mut owner, &project).await.role_skills,
        RoleSkills::default(),
        "All approved skills is every role's setting until the owner picks"
    );
    let agents = private_directory(&case.root.path().join("agents"));
    for role in SkillRole::ALL {
        let home = private_directory(&agents.join(format!("{role:?}-default")));
        assert_eq!(role_launch(&case, &project, role, &home), every, "{role:?}");
    }

    let worker_chose = RoleSkills {
        worker: Some(owned(&["meal-tips"])),
        ..RoleSkills::default()
    };
    let kept = save_coordination(
        &mut owner,
        &project,
        CoordSettings {
            role_skills: worker_chose.clone(),
            ..CoordSettings::default()
        },
        2,
    )
    .await
    .unwrap();
    assert_eq!(kept.role_skills, worker_chose);
    let back = save_coordination(&mut owner, &project, CoordSettings::default(), 3)
        .await
        .unwrap();
    assert_eq!(
        back.role_skills,
        RoleSkills::default(),
        "choosing All approved skills again clears the workers' list"
    );
    for role in SkillRole::ALL {
        let home = private_directory(&agents.join(format!("{role:?}-again")));
        assert_eq!(role_launch(&case, &project, role, &home), every, "{role:?}");
    }
    drop(owner);
    case.close().await;
}

#[tokio::test]
async fn with_chosen_skills_each_role_gets_its_own_list() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let project = crew_with_skills(
        &case,
        &mut owner,
        &["meal-tips", "release-notes", "style-guide"],
    )
    .await;
    let chosen = RoleSkills {
        master: Some(owned(&["meal-tips"])),
        lead: Some(Vec::new()),
        worker: Some(owned(&["style-guide", "release-notes", "not-approved"])),
    };
    let kept = save_coordination(
        &mut owner,
        &project,
        CoordSettings {
            role_skills: chosen.clone(),
            ..CoordSettings::default()
        },
        2,
    )
    .await
    .unwrap();
    assert_eq!(kept.role_skills, chosen);
    let agents = private_directory(&case.root.path().join("agents"));
    for (role, given) in [
        (SkillRole::Master, owned(&["meal-tips"])),
        (SkillRole::Lead, Vec::new()),
        (SkillRole::Worker, owned(&["release-notes", "style-guide"])),
    ] {
        let home = private_directory(&agents.join(format!("{role:?}")));
        assert_eq!(
            role_launch(&case, &project, role, &home),
            BTreeSet::from_iter(given),
            "{role:?}: only its chosen names that are approved skills"
        );
    }
    assert_eq!(
        save_coordination(
            &mut owner,
            &project,
            CoordSettings {
                role_skills: RoleSkills {
                    worker: Some(owned(&["Not A Skill"])),
                    ..RoleSkills::default()
                },
                ..CoordSettings::default()
            },
            3,
        )
        .await
        .err(),
        Some("coord_settings_invalid".into()),
        "a name no skill can have is refused"
    );
    assert_eq!(
        coordination(&mut owner, &project).await.role_skills,
        chosen,
        "a refused choice changes nothing"
    );
    drop(owner);
    case.close().await;
}

/// The team section a master with tools off reads (DEC-111 R3). Takes the case and the
/// project. Returns the section between its heading and the landing rules.
fn team_section(case: &Case, project: &ProjectId) -> String {
    let (_, text) = case.master(project, false).unwrap();
    let Some((_, section)) = text.split_once("\n## Team skills and MCP servers\n\n") else {
        return String::new();
    };
    section
        .split_once("\n## Landing the team's work")
        .unwrap()
        .0
        .to_owned()
}

#[tokio::test]
async fn a_master_with_tools_off_hears_which_team_roles_get_each_skill() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let project = crew_with_skills(
        &case,
        &mut owner,
        &["meal-tips", "release-notes", "style-guide"],
    )
    .await;
    let tools_off = "Your own tools are off in this conversation, so you cannot call these.";
    assert_eq!(
        team_section(&case, &project),
        format!(
            "{tools_off} The owner approved them for this project, and every team agent with tools on receives them. Plan the team's work with them in mind.\n\n- Skill meal-tips: Teaches meal-tips.\n- Skill release-notes: Teaches release-notes.\n- Skill style-guide: Teaches style-guide.\n"
        ),
        "with All approved skills for every role, every team agent gets every skill"
    );
    let per_role = format!(
        "{tools_off} The owner approved them for this project. Every team agent with tools on receives the MCP servers, but each skill only the team roles named after it, as the owner chose: a lead is a sub-agent that may start sub-agents of its own, and a worker is any other sub-agent or teammate. Plan the team's work with them in mind.\n\n"
    );
    save_coordination(
        &mut owner,
        &project,
        CoordSettings {
            role_skills: RoleSkills {
                master: Some(Vec::new()),
                lead: Some(owned(&["meal-tips", "not-approved"])),
                worker: Some(owned(&["release-notes", "meal-tips"])),
            },
            ..CoordSettings::default()
        },
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        team_section(&case, &project),
        format!(
            "{per_role}- Skill meal-tips (leads and workers): Teaches meal-tips.\n- Skill release-notes (workers): Teaches release-notes.\n"
        ),
        "each skill names the roles the owner chose for it, and one no team agent gets is left out"
    );
    save_coordination(
        &mut owner,
        &project,
        CoordSettings {
            role_skills: RoleSkills {
                worker: Some(Vec::new()),
                ..RoleSkills::default()
            },
            ..CoordSettings::default()
        },
        3,
    )
    .await
    .unwrap();
    assert_eq!(
        team_section(&case, &project),
        format!(
            "{per_role}- Skill meal-tips (leads): Teaches meal-tips.\n- Skill release-notes (leads): Teaches release-notes.\n- Skill style-guide (leads): Teaches style-guide.\n"
        ),
        "leads on All approved skills get every skill while workers get none"
    );
    save_coordination(
        &mut owner,
        &project,
        CoordSettings {
            role_skills: RoleSkills {
                lead: Some(Vec::new()),
                worker: Some(Vec::new()),
                ..RoleSkills::default()
            },
            ..CoordSettings::default()
        },
        4,
    )
    .await
    .unwrap();
    assert_eq!(
        team_section(&case, &project),
        "",
        "with no skill for any team agent and no MCP server, the master hears no team section"
    );
    drop(owner);
    case.close().await;
}

#[tokio::test]
async fn every_master_and_agent_launch_states_how_work_lands() {
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let root = case.root.path().to_path_buf();
    let project = ProjectId::new("landing").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Landing".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Lands work through Changes.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap(),
    );
    let agent_rules = [
        "Never create, switch or delete a branch, never commit, push or tag, and never write inside `.git`.",
        "If the sandbox refuses a command, never retry it outside the sandbox",
        "Leave your edits as files in your worktree.",
        "the owner accepts it in the project's Changes view onto the branch checked out in the project folder",
    ];
    let master_rules = [
        "Never tell a sub-agent how to branch, commit or push",
        "never name a base branch in a task",
        "tell the owner to review and accept them in the project's Changes view",
    ];
    for tools in [false, true] {
        let (_, master) = case.master(&project, tools).unwrap();
        for rule in agent_rules.iter().chain(&master_rules) {
            assert!(
                master.contains(rule),
                "master, tools {tools}, misses: {rule}"
            );
        }
    }

    let agents = private_directory(&root.join("agents"));
    let own_branch_rules = [
        "Commit your work on the branch checked out in your worktree with conventional commit messages before your task ends",
        "never write inside the project folder's `.git`",
        "the owner's Accept in the project's Changes view lands your commits",
    ];
    for (layout, told, never) in [
        (Layout::Linked, &agent_rules[..], &own_branch_rules[..]),
        (
            Layout::Clone,
            &own_branch_rules[..],
            &[
                "never commit, push or tag",
                "Leave your edits as files in your worktree.",
            ][..],
        ),
    ] {
        let context = case.context(&project, layout).unwrap();
        let codex_home = private_directory(&agents.join(format!("codex-home-{layout:?}")));
        let (_, codex_files) =
            command_with_context(&request(&case, "codex", &codex_home, false), &context).unwrap();
        let config: toml::Table = fs::read_to_string(codex_home.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        let developer = config["developer_instructions"]
            .as_str()
            .unwrap()
            .to_owned();
        let claude_home = private_directory(&agents.join(format!("claude-home-{layout:?}")));
        let (claude_command, claude_files) = command_with_context(
            &request(&case, "claude-code", &claude_home, false),
            &context,
        )
        .unwrap();
        let prompt = fs::read_to_string(
            argument_after(&claude_command, "--append-system-prompt-file").unwrap(),
        )
        .unwrap();
        for (harness, text) in [
            ("codex", developer.as_str()),
            ("claude-code", prompt.as_str()),
        ] {
            for rule in told {
                assert!(text.contains(rule), "{harness} {layout:?} misses: {rule}");
            }
            for rule in never.iter().chain(&master_rules) {
                assert!(!text.contains(rule), "{harness} {layout:?} is told: {rule}");
            }
        }
        drop(codex_files);
        drop(claude_files);
    }
    drop(owner);
    case.close().await;
}

/// L4 (DEC-147.9, DEC-141): every master and sub-agent launch, in a linked worktree or its own
/// clone, names the project folder and what each shared folders choice gives, keeps secret
/// files closed and says toolchains are never installed, for every scope and access option
/// the owner saves.
#[tokio::test]
async fn every_launch_names_the_shared_folders_for_each_choice() {
    use crazyagent::sandbox::policy::AgentPolicy;
    use crazyagent::sandbox::shared_folders::{SharedAccess, SharedFolders, SharedScope};
    let case = Case::new().await;
    let mut owner = case.owner().await;
    let project = ProjectId::new("shared").unwrap();
    applied(
        owner
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Shared".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Shares local folders.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap(),
    );
    let options = [
        (
            SharedScope::All,
            SharedAccess::ReadOnly,
            "holds folders your worktree does not have",
            Some("They are read-only to you."),
        ),
        (
            SharedScope::All,
            SharedAccess::ReadWrite,
            "holds folders your worktree does not have",
            Some("You may change files in the shared top-level folders"),
        ),
        (
            SharedScope::Chosen,
            SharedAccess::ReadOnly,
            "only these are shared with you: `program`",
            Some("They are read-only to you."),
        ),
        (
            SharedScope::None,
            SharedAccess::ReadOnly,
            "are not shared with you. Work in your worktree.",
            None,
        ),
    ];
    for (generation, (scope, access, told, how)) in (2u64..).zip(options) {
        let view = owner.project_agent_policy(project.clone()).await.unwrap();
        let policy = AgentPolicy {
            access: view.access,
            network: view.network,
            shared_folders: SharedFolders {
                scope,
                access,
                chosen: vec!["program".into()],
                secrets: Vec::new(),
            },
            ..AgentPolicy::default()
        };
        applied(
            owner
                .set_project_agent_policy(project.clone(), view.version, policy, generation)
                .await
                .unwrap(),
        );
        let saved = owner.project_agent_policy(project.clone()).await.unwrap();
        assert_eq!(
            (saved.shared_folders.scope, saved.shared_folders.access),
            (scope, access)
        );
        let mut texts = Vec::new();
        for tools in [false, true] {
            texts.push(case.master(&project, tools).unwrap().1);
        }
        for layout in [Layout::Linked, Layout::Clone] {
            texts.push(
                case.context(&project, layout)
                    .unwrap()
                    .instructions()
                    .to_owned(),
            );
        }
        for text in &texts {
            assert!(
                text.contains("## Shared project folders"),
                "{scope:?} {access:?}: {text}"
            );
            assert!(
                text.contains(told),
                "{scope:?} {access:?} misses {told}: {text}"
            );
            if let Some(how) = how {
                assert!(text.contains(how), "{scope:?} {access:?} misses {how}");
            }
            assert!(text.contains("Secret files there"), "{scope:?} {access:?}");
            assert!(
                text.contains("Never install or download a toolchain"),
                "{scope:?} {access:?}"
            );
        }
    }
    drop(owner);
    case.close().await;
}
