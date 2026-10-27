#![cfg(target_os = "macos")]

use crazyagent::agent_home::{self, Harness};
use crazyagent::client_controller::OwnerController;
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

    fn context(&self, project: &ProjectId) -> Result<AgentContext, String> {
        let connection = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        launch_context::for_project(&connection, project).map_err(|problem| problem.0)
    }

    fn master(&self, project: &ProjectId, tools: bool) -> Result<(bool, String), String> {
        let connection = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        let switch = launch_context::master_tools(&tx, project).map_err(|problem| problem.0)?;
        let text = launch_context::for_master(&tx, project, tools).map_err(|problem| problem.0)?;
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
        .switch_plugin(project.clone(), switched.id.clone(), false)
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

    let context = case.context(&project).unwrap();
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
    let received = |home: &Path, harness| {
        let received = agent_home::received(home, harness).unwrap().unwrap();
        received.skills.into_iter().collect::<BTreeSet<_>>()
    };
    assert_eq!(
        received(&codex_home, Harness::Codex),
        BTreeSet::from(["meal-tips".to_owned(), "release-notes".to_owned()]),
        "the skill names of the last launch outlive its copies"
    );
    assert_eq!(
        received(&claude_home, Harness::Claude),
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
        received(&off_home, Harness::Codex),
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

    let other_context = case.context(&other).unwrap();
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
        case.context(&ProjectId::new("ghost").unwrap()).err(),
        Some("plugin_project_unknown".into())
    );
    drop(owner);
    case.close().await;
}
