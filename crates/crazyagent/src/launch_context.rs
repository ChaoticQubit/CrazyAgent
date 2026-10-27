use crate::harnesses::launch::LaunchError;
use crate::plugins::{
    EffectivePlugins, EffectiveSkill, LaunchPlugins, LaunchSecret, LaunchServer, ServerSpec,
};
use crate::project_setup::{ContextKind, StoredSetup};
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction};
#[cfg(unix)]
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const HEADER_BYTES_MAX: usize = HEADER_TITLE.len()
    + "You are working on the project \"\".\n".len()
    + NAME_BYTES_MAX
    + 2 * ("Project folder: \n".len() + FOLDER_BYTES_MAX);
pub const BRIEF_BYTES_MAX: usize = crate::project_setup::CONTEXT_CHUNK_BYTES;
pub const INDEX_ITEMS_MAX: usize = crate::project_setup::CONTEXT_ITEMS_MAX;
pub const INDEX_BYTES_MAX: usize = 7 * 1024;
pub const MEMORY_ITEMS_MAX: usize = crate::memory::library::LAUNCH_ITEMS_MAX;
pub const MEMORY_SECTION_BYTES_MAX: usize = crate::memory::library::LAUNCH_BYTES_MAX + 1024;
pub const INSTRUCTIONS_BYTES_MAX: usize = 32 * 1024;
pub const SKILLS_INDEX_BYTES_MAX: usize = 8 * 1024;
pub const TEAM_INDEX_BYTES_MAX: usize = 4 * 1024;
pub const MASTER_INSTRUCTIONS_BYTES_MAX: usize =
    INSTRUCTIONS_BYTES_MAX + TEAM_INDEX_BYTES_MAX + SECTION_SLACK;
pub const SKILL_FILES_MAX: usize = 4096;
pub const SKILL_BYTES_MAX: u64 = 64 * 1024 * 1024;
const SKILL_DEPTH_MAX: usize = 8;
const SECTION_SLACK: usize = 512;
const NAME_BYTES_MAX: usize = 256;
const DESCRIPTION_BYTES_MAX: usize = 512;
const FOLDER_BYTES_MAX: usize = 1024;
const HEADER_TITLE: &str = "# Project context from CrazyAgent\n\n";
const STATE: &str = "crazyagent";
#[cfg(unix)]
const LOCK: &str = "crazyagent.lock";
const PLUGIN_MANIFEST: &str = "{\"name\":\"crazyagent\",\"version\":\"1.0.0\",\"description\":\"Skills the owner approved for this project in CrazyAgent\"}\n";

const _: () = assert!(
    HEADER_BYTES_MAX
        + BRIEF_BYTES_MAX
        + INDEX_BYTES_MAX
        + MEMORY_SECTION_BYTES_MAX
        + 3 * SECTION_SLACK
        <= INSTRUCTIONS_BYTES_MAX
);
const _: () = assert!(
    MASTER_INSTRUCTIONS_BYTES_MAX <= crate::adapters::codex::DEVELOPER_INSTRUCTIONS_BYTES_MAX
);

fn failure(code: &str) -> LaunchError {
    LaunchError(code.into())
}

fn unavailable<E>(_: E) -> LaunchError {
    failure("launch_context_unavailable")
}

fn skill_unavailable<E>(_: E) -> LaunchError {
    failure("launch_context_skill_unavailable")
}

/// What every agent the engine launches for one project receives: the approved skills and
/// MCP servers with their secret values, and one bounded startup instruction holding the
/// project brief, an index of its context files and the owner-approved memories.
#[derive(Debug)]
pub struct AgentContext {
    plugins: LaunchPlugins,
    instructions: String,
    process: Option<Box<Process>>,
}

/// The owner's app and project instructions one launch writes into the agent's home,
/// with the project they belong to.
#[derive(Debug)]
struct Process {
    project: crate::agent_home::AgentProject,
    text: String,
}

/// The files one launch wrote into its agent home, and the lock that keeps a second launch
/// out of that home. Dropping it deletes every file holding a secret and the copied skills.
#[derive(Debug)]
pub struct Installed {
    arguments: Vec<OsString>,
    removals: Vec<PathBuf>,
    copies: Option<PathBuf>,
    _lock: File,
}

struct Budget {
    files: usize,
    bytes: u64,
}

/// Reads what agents launched for one project receive, in one read transaction.
/// Input: a connection to the engine's `engine.db` (the engine root is its folder) and the
/// project. Output: the context, or a plain code such as `plugin_project_unknown`.
pub fn for_project(
    connection: &Connection,
    project: &ProjectId,
) -> Result<AgentContext, LaunchError> {
    let root = engine_root(connection)?;
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    let plugins = crate::storage::plugins::launch_set(&tx, &root, project)
        .map_err(|problem| LaunchError(problem.0))?;
    let instructions = project_instructions(&tx, project)?;
    let home_project = agent_project(&tx, project)?;
    drop(tx);
    let text = crate::agent_home::text(&home_project).map_err(|problem| LaunchError(problem.0))?;
    Ok(AgentContext {
        plugins,
        instructions,
        process: Some(Box::new(Process {
            project: home_project,
            text,
        })),
    })
}

/// Names what an agent home of the project receives the owner's instructions from: the
/// engine data folder (the folder of `engine.db`) and the project's recorded folder.
/// Input: a transaction on `engine.db` and the project. Output: the project, or a plain code.
pub(crate) fn agent_project(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<crate::agent_home::AgentProject, LaunchError> {
    let folder = crate::project_setup::load_setup(tx, project)
        .map_err(unavailable)?
        .map(|setup| setup.project_folder())
        .filter(|folder| folder.is_absolute());
    Ok(crate::agent_home::AgentProject {
        data: engine_root(tx)?,
        id: project.clone(),
        folder,
    })
}

/// Reads the project's master tools switch: the team plan's `master_tools`, the value the
/// Team settings write. A project without a stored setup has no switch and runs tools off.
/// Input: a transaction on `engine.db` and the project. Output: the switch, or a plain code.
pub fn master_tools(tx: &Transaction<'_>, project: &ProjectId) -> Result<bool, LaunchError> {
    Ok(crate::project_setup::load_setup(tx, project)
        .map_err(unavailable)?
        .is_some_and(|setup| setup.team.master_tools))
}

/// Builds the startup instruction for the engine's own project master conversation: the
/// same project context and approved memories every agent gets, then with tools on what
/// its tools are and how it starts a sub-agent (DEC-093), or with tools off the names of
/// the approved skills and MCP servers that team agents with tools on receive. No secret
/// value, command, URL or file of a server is read or given. Input: a transaction the caller
/// holds on `engine.db`, the project, its master tools switch and the live model access,
/// none without settings. Output: at most `MASTER_INSTRUCTIONS_BYTES_MAX` bytes of markdown,
/// or a plain code.
pub fn for_master(
    tx: &Transaction<'_>,
    project: &ProjectId,
    tools: bool,
    access: Option<&crate::config::ModelAccess>,
) -> Result<String, LaunchError> {
    let root = engine_root(tx)?;
    let mut text = project_instructions(tx, project)?;
    if tools {
        let setup = crate::project_setup::load_setup(tx, project)
            .map_err(unavailable)?
            .ok_or_else(|| failure("launch_context_master_tools_without_plan"))?;
        let available = access
            .map(|access| crate::team::available_harnesses(access, project))
            .unwrap_or_default();
        text.push_str(&master_tools_section(&setup, &available));
    } else {
        let plugins = crate::storage::plugins::effective(tx, &root, project)
            .map_err(|problem| LaunchError(problem.0))?;
        text.push_str(&team_index(&plugins));
    }
    assert!(text.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
    Ok(text)
}

/// Tells a conversation master whose tools are on what it can do and how it starts a
/// sub-agent: its worktree is the only folder it writes, its `delegate` tool hands one task
/// to the team, and the team plan's sub-agents, or for an empty plan the harnesses it can use.
/// Input: the project setup and the harnesses with an eligible account now.
/// Output: at most `TEAM_INDEX_BYTES_MAX` plus slack bytes.
pub(crate) fn master_tools_section(setup: &StoredSetup, available: &[&str]) -> String {
    let mut section = format!(
        "\n## Your tools\n\nYour tools are on in this conversation. You can read and search files, edit them and run shell commands in this conversation's own git worktree, your working folder. You cannot write outside it; the project folder itself stays read-only to you.\n\nTo start a sub-agent, call the `{}` tool of the `{}` MCP server with the task in plain words and, if you choose, a harness from the team plan. Each call hands over one task, at most {} in one reply. Start sub-agents only with this tool. Never use your harness's own sub-agent tools, such as Codex's `spawn_agent`, `followup_task`, `send_message` or `wait_agent`: CrazyAgent stops any agent they start, and only agents started with this tool run on the project's pool and appear in the project's Agents tab and Team view. When your reply ends, CrazyAgent checks each task against the team plan and runs it in its own git worktree. Its full progress and result show in the project's Team view. When it ends, a short note on how it went comes back to this conversation as an `Engine:` line you see on your next turn.\n\n",
        crate::team::tool::TOOL_NAME,
        crate::team::tool::SERVER_NAME,
        crate::team::DELEGATIONS_PER_REPLY_MAX
    );
    if setup.team.sub_harnesses.is_empty() {
        section.push_str(&crate::team::any_harness_text(&setup.team, available));
    } else {
        section.push_str("The team plan's sub-agents:\n");
        let mut left = 0;
        for slot in &setup.team.sub_harnesses {
            let line = format!(
                "- {}: up to {} agents, tools {}\n",
                clip(&slot.harness, NAME_BYTES_MAX),
                slot.agents,
                if slot.tools { "on" } else { "off" }
            );
            if left > 0 || section.len() + line.len() > TEAM_INDEX_BYTES_MAX {
                left += 1;
                continue;
            }
            section.push_str(&line);
        }
        if left > 0 {
            section.push_str(&format!("- and {left} more\n"));
        }
    }
    assert!(section.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK);
    section
}

fn engine_root(connection: &Connection) -> Result<PathBuf, LaunchError> {
    Ok(connection
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .and_then(Path::parent)
        .filter(|root| root.is_absolute())
        .ok_or_else(|| failure("launch_context_database_unknown"))?
        .to_path_buf())
}

fn project_instructions(tx: &Transaction<'_>, project: &ProjectId) -> Result<String, LaunchError> {
    let setup = crate::project_setup::load_setup(tx, project).map_err(unavailable)?;
    let memories = crate::memory::library::approved_for_project(tx, project)
        .map_err(unavailable)?
        .render();
    let instructions = render(project, setup.as_ref(), &memories);
    assert!(instructions.len() <= INSTRUCTIONS_BYTES_MAX);
    Ok(instructions)
}

impl AgentContext {
    /// Returns the startup instruction every harness receives, at most
    /// `INSTRUCTIONS_BYTES_MAX` bytes of markdown.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Returns the approved skills and MCP servers given to agents whose tools are on.
    pub fn plugins(&self) -> &LaunchPlugins {
        &self.plugins
    }

    /// Lists what a project sandbox must let an agent with tools on read beyond its home:
    /// the executable of every approved command server.
    pub fn read_paths(&self) -> Vec<PathBuf> {
        self.plugins
            .servers
            .iter()
            .filter_map(|server| match &server.spec {
                ServerSpec::Stdio { command, .. } => Some(PathBuf::from(command)),
                ServerSpec::Http { .. } => None,
            })
            .collect()
    }

    /// Adds the engine's own MCP server that gives a team master with tools on its `delegate` tool.
    /// Input: the absolute canonical engine executable that serves it and the absolute drop file
    /// in the master's private temporary folder. Output: the context, or a plain code when a path
    /// is not plain or an approved server already uses the name.
    pub fn with_team_tool(self, executable: &Path, drop: &Path) -> Result<Self, LaunchError> {
        self.with_team_server(executable, crate::team::tool::SERVER_COMMAND, drop)
    }

    /// Adds the engine's own MCP server that gives a live team master with tools on its team
    /// tools, answered by the engine at once through the mailbox folder. Input: the absolute
    /// canonical engine executable and the absolute mailbox in the master's private temporary
    /// folder. Output: the context, or a plain code as for [`Self::with_team_tool`].
    pub fn with_live_team_tool(
        self,
        executable: &Path,
        mailbox: &Path,
    ) -> Result<Self, LaunchError> {
        self.with_team_server(executable, crate::team::tool::LIVE_SERVER_COMMAND, mailbox)
    }

    fn with_team_server(
        mut self,
        executable: &Path,
        subcommand: &str,
        drop: &Path,
    ) -> Result<Self, LaunchError> {
        let plain = |path: &Path| {
            path.is_absolute()
                .then(|| path.to_str())
                .flatten()
                .filter(|text| !text.chars().any(char::is_control))
                .map(str::to_owned)
        };
        let (Some(command), Some(drop)) = (plain(executable), plain(drop)) else {
            return Err(failure("launch_context_team_tool_invalid"));
        };
        let name = crate::team::tool::SERVER_NAME;
        if self
            .plugins
            .servers
            .iter()
            .any(|server| server.name == name)
        {
            return Err(failure("launch_context_team_tool_taken"));
        }
        self.plugins.servers.push(LaunchServer {
            name: name.into(),
            spec: ServerSpec::Stdio {
                command,
                args: vec![subcommand.into(), drop],
            },
            secrets: Vec::new(),
        });
        Ok(self)
    }

    /// Adds the names of the project's approved skills and MCP servers for a team master
    /// whose own tools are off, so it plans the team's work with them. No secret value,
    /// command, URL or file of a server is read or given. Input: a connection to the
    /// engine's `engine.db` and the project. Output: the context, or a plain code.
    pub fn with_team_index(
        self,
        connection: &Connection,
        project: &ProjectId,
    ) -> Result<Self, LaunchError> {
        let root = engine_root(connection)?;
        let tx = connection.unchecked_transaction().map_err(unavailable)?;
        let plugins = crate::storage::plugins::effective(&tx, &root, project)
            .map_err(|problem| LaunchError(problem.0))?;
        drop(tx);
        Ok(self.with_index(&plugins))
    }

    fn with_index(mut self, plugins: &EffectivePlugins) -> Self {
        self.instructions.push_str(&team_index(plugins));
        assert!(self.instructions.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
        self
    }

    /// Reports whether any MCP secret value occurs in `text`, so launchers can prove a
    /// command line or environment never carries one.
    pub fn exposes_secret(&self, text: &[u8]) -> bool {
        self.plugins.servers.iter().any(|server| {
            server.secrets.iter().any(|secret| {
                let value = secret.value.expose();
                !value.is_empty() && text.windows(value.len()).any(|window| window == value)
            })
        })
    }

    /// Writes this context into one agent's private home for one launch of `harness`.
    /// Input: the harness id ("codex" or "claude-code"), the agent's 0700 home (its
    /// CODEX_HOME or CLAUDE_CONFIG_DIR) and the tools switch; with tools off no skill or
    /// MCP server is given. Output: the guard holding the files and the harness arguments,
    /// or `launch_home_busy` while another launch uses the home.
    pub fn install(
        &self,
        harness: &str,
        home: &Path,
        tools: bool,
    ) -> Result<Installed, LaunchError> {
        let id = crate::harnesses::spec(harness)
            .map(|spec| spec.id)
            .filter(|id| matches!(*id, "codex" | "claude-code"))
            .ok_or_else(|| failure("launch_harness_unsupported"))?;
        crate::harnesses::launch::validate_home(home)?;
        let lock = lock(home)?;
        let state = home.join(STATE);
        let mut installed = Installed {
            arguments: Vec::new(),
            removals: vec![state.clone()],
            copies: None,
            _lock: lock,
        };
        remove(&state);
        directory(&state, true)?;
        if let Some(process) = &self.process {
            let harness = crate::agent_home::Harness::from_id(id)
                .ok_or_else(|| failure("launch_harness_unsupported"))?;
            crate::agent_home::prepare(home, harness, &process.project, &process.text)
                .map_err(|problem| LaunchError(problem.0))?;
        }
        let servers: &[LaunchServer] = if tools { &self.plugins.servers } else { &[] };
        let skills = tools && !self.plugins.skills.is_empty();
        let names: Vec<&str> = match skills {
            true => self
                .plugins
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect(),
            false => Vec::new(),
        };
        crate::agent_home::record_skills(home, &names).map_err(|problem| LaunchError(problem.0))?;
        if id == "codex" {
            let config = home.join("config.toml");
            let copies = home.join("skills").join(STATE);
            installed.removals.push(config.clone());
            installed.copies = Some(home.to_path_buf());
            remove_copies(home);
            if skills {
                directory(&home.join("skills"), false)?;
                self.copy_skills(&copies)?;
            }
            remove(&config);
            write_secret(&config, codex_config(&self.instructions, servers)?)?;
        } else {
            let mcp = state.join("mcp.json");
            write_secret(&mcp, claude_config(servers)?)?;
            installed
                .arguments
                .extend([OsString::from("--mcp-config"), mcp.into_os_string()]);
            let mut text = self.instructions.clone();
            if let Some(process) = self
                .process
                .as_ref()
                .filter(|process| !process.text.is_empty())
            {
                text.push('\n');
                text.push_str(&process.text);
            }
            if skills {
                let plugin = state.join("plugin");
                directory(&plugin, true)?;
                directory(&plugin.join(".claude-plugin"), true)?;
                write_private(
                    &plugin.join(".claude-plugin").join("plugin.json"),
                    PLUGIN_MANIFEST.as_bytes(),
                )?;
                self.copy_skills(&plugin.join("skills"))?;
                text.push_str(&skills_index(&plugin.join("skills"), &self.plugins.skills));
                installed
                    .arguments
                    .extend([OsString::from("--plugin-dir"), plugin.into_os_string()]);
            }
            assert!(
                text.len()
                    <= INSTRUCTIONS_BYTES_MAX
                        + SKILLS_INDEX_BYTES_MAX
                        + SECTION_SLACK
                        + crate::agent_home::PROCESS_BYTES_MAX
            );
            let instructions = state.join("instructions.md");
            write_private(&instructions, text.as_bytes())?;
            installed.arguments.extend([
                OsString::from("--append-system-prompt-file"),
                instructions.into_os_string(),
            ]);
        }
        assert!(installed.removals.iter().all(|path| path.starts_with(home)));
        assert!(installed.arguments.len() <= 6);
        Ok(installed)
    }

    fn copy_skills(&self, target: &Path) -> Result<(), LaunchError> {
        directory(target, true)?;
        let mut budget = Budget { files: 0, bytes: 0 };
        for skill in &self.plugins.skills {
            let name = &skill.name;
            if name.is_empty()
                || name.len() > 64
                || name.starts_with('.')
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return Err(failure("launch_context_skill_invalid"));
            }
            copy_tree(&skill.folder, &target.join(name), 0, &mut budget)?;
        }
        assert!(budget.files <= SKILL_FILES_MAX && budget.bytes <= SKILL_BYTES_MAX);
        Ok(())
    }
}

impl Installed {
    /// Returns the arguments that point the harness at the generated files, in order.
    /// Claude Code: `--mcp-config <file> [--plugin-dir <dir>] --append-system-prompt-file
    /// <file>`. Codex: none, it reads `config.toml` and `skills/` from its CODEX_HOME.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

impl Drop for Installed {
    /// Deletes the launch's files, then releases the home lock on its open file
    /// description, so a process that shares it, such as a child another thread is
    /// starting, can never keep the home busy after the launch ends.
    fn drop(&mut self) {
        for path in &self.removals {
            remove(path);
        }
        if let Some(home) = &self.copies {
            remove_copies(home);
        }
        #[cfg(unix)]
        let _ = rustix::fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

fn clip(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
        _ => format!("{} MB", bytes.div_ceil(1_048_576)),
    }
}

fn render(project: &ProjectId, setup: Option<&StoredSetup>, memories: &str) -> String {
    let mut header = String::from(HEADER_TITLE);
    let mut brief = String::new();
    let mut index = String::new();
    match setup {
        None => header.push_str(&format!(
            "You are working on the project {}.\n",
            project.as_str()
        )),
        Some(setup) => {
            header.push_str(&format!(
                "You are working on the project \"{}\".\n",
                clip(&setup.name, NAME_BYTES_MAX)
            ));
            let project_folder = setup.project_folder();
            let state = setup.state_folder();
            for (label, folder) in [
                ("Project folder", &project_folder),
                ("Context folder", &state),
            ] {
                if folder.as_os_str().len() <= FOLDER_BYTES_MAX {
                    header.push_str(&format!("{label}: {}\n", folder.display()));
                }
            }
            if let Ok(chunk) = crate::project_setup::read_context(setup, "context/brief.md", 0)
                && let Some(text) = chunk.text
            {
                brief.push_str("\n## Brief\n\n");
                brief.push_str(clip(text.trim_end(), BRIEF_BYTES_MAX));
                brief.push('\n');
                if chunk.next_offset.is_some() || text.len() > BRIEF_BYTES_MAX {
                    brief.push_str("\nThe brief continues in context/brief.md.\n");
                }
            }
            if let Ok(context) = crate::project_setup::list_context(setup)
                && !context.items.is_empty()
            {
                index.push_str("\n## Context files\n\nThese paths are relative to the context folder. Open them when they help with your task.\n\n");
                let mut left = 0;
                for item in context.items.iter().take(INDEX_ITEMS_MAX) {
                    let kind = match item.kind {
                        ContextKind::Brief => "brief",
                        ContextKind::Document => "document",
                        ContextKind::Research => "research",
                    };
                    let line = format!("- {} ({kind}, {})\n", item.path, size(item.bytes));
                    if left > 0 || index.len() + line.len() > INDEX_BYTES_MAX {
                        left += 1;
                        continue;
                    }
                    index.push_str(&line);
                }
                left += context.items.len().saturating_sub(INDEX_ITEMS_MAX);
                if left > 0 {
                    index.push_str(&format!("- and {left} more files in context/\n"));
                }
            }
        }
    }
    assert!(header.len() <= HEADER_BYTES_MAX);
    assert!(brief.len() <= BRIEF_BYTES_MAX + SECTION_SLACK);
    assert!(index.len() <= INDEX_BYTES_MAX + SECTION_SLACK);
    assert!(memories.len() <= MEMORY_SECTION_BYTES_MAX);
    let mut text = header + &brief + &index;
    if !memories.is_empty() {
        text.push('\n');
        text.push_str(memories);
    }
    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX);
    text
}

fn skill_description(path: &Path) -> String {
    let mut text = String::new();
    File::open(path)
        .and_then(|file| file.take(64 * 1024).read_to_string(&mut text))
        .ok()
        .and_then(|_| crate::plugins::skill_folder::frontmatter(&text).ok())
        .map(|(_, description)| description)
        .unwrap_or_default()
}

fn skills_index(folder: &Path, skills: &[EffectiveSkill]) -> String {
    let mut index = String::from(
        "\n## Skills\n\nThe owner approved these skills for this project. Read a skill's SKILL.md before you use it.\n\n",
    );
    let mut left = 0;
    for skill in skills {
        let path = folder.join(&skill.name).join("SKILL.md");
        let description = skill_description(&path);
        let line = format!("- {}: {} ({})\n", skill.name, description, path.display());
        if left > 0 || index.len() + line.len() > SKILLS_INDEX_BYTES_MAX {
            left += 1;
            continue;
        }
        index.push_str(&line);
    }
    if left > 0 {
        index.push_str(&format!("- and {left} more skills in the same folder\n"));
    }
    assert!(index.len() <= SKILLS_INDEX_BYTES_MAX + SECTION_SLACK);
    index
}

fn team_index(plugins: &EffectivePlugins) -> String {
    if plugins.skills.is_empty() && plugins.servers.is_empty() {
        return String::new();
    }
    let mut index = String::from(
        "\n## Team skills and MCP servers\n\nYour own tools are off in this conversation, so you cannot call these. The owner approved them for this project, and every team agent with tools on receives them. Plan the team's work with them in mind.\n\n",
    );
    let skills = plugins.skills.iter().map(|skill| {
        let description = skill_description(&skill.folder.join("SKILL.md"));
        format!(
            "- Skill {}: {}\n",
            clip(&skill.name, NAME_BYTES_MAX),
            clip(description.trim(), DESCRIPTION_BYTES_MAX)
        )
    });
    let servers = plugins
        .servers
        .iter()
        .map(|server| format!("- MCP server {}\n", clip(&server.name, NAME_BYTES_MAX)));
    let mut left = 0;
    for line in skills.chain(servers) {
        if left > 0 || index.len() + line.len() > TEAM_INDEX_BYTES_MAX {
            left += 1;
            continue;
        }
        index.push_str(&line);
    }
    if left > 0 {
        index.push_str(&format!("- and {left} more\n"));
    }
    assert!(index.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK);
    index
}

fn secret_text(secret: &LaunchSecret) -> Result<&str, LaunchError> {
    std::str::from_utf8(secret.value.expose())
        .ok()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .ok_or_else(|| failure("launch_context_secret_invalid"))
}

fn codex_config(instructions: &str, servers: &[LaunchServer]) -> Result<String, LaunchError> {
    let mut document = toml_edit::DocumentMut::new();
    document["developer_instructions"] = toml_edit::value(instructions);
    if !servers.is_empty() {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        for server in servers {
            let mut values = toml_edit::InlineTable::new();
            for secret in &server.secrets {
                values.insert(secret.name.as_str(), secret_text(secret)?.into());
            }
            let mut entry = toml_edit::Table::new();
            match &server.spec {
                ServerSpec::Stdio { command, args } => {
                    entry["command"] = toml_edit::value(command.as_str());
                    entry["args"] = toml_edit::value(
                        args.iter()
                            .map(String::as_str)
                            .collect::<toml_edit::Array>(),
                    );
                    if !values.is_empty() {
                        entry["env"] = toml_edit::value(values);
                    }
                }
                ServerSpec::Http { url } => {
                    entry["url"] = toml_edit::value(url.as_str());
                    if !values.is_empty() {
                        entry["http_headers"] = toml_edit::value(values);
                    }
                }
            }
            table.insert(&server.name, toml_edit::Item::Table(entry));
        }
        document["mcp_servers"] = toml_edit::Item::Table(table);
    }
    Ok(document.to_string())
}

fn claude_config(servers: &[LaunchServer]) -> Result<String, LaunchError> {
    let mut declared = serde_json::Map::new();
    for server in servers {
        let mut values = serde_json::Map::new();
        for secret in &server.secrets {
            values.insert(secret.name.clone(), secret_text(secret)?.into());
        }
        let entry = match &server.spec {
            ServerSpec::Stdio { command, args } => serde_json::json!({
                "type": "stdio", "command": command, "args": args, "env": values
            }),
            ServerSpec::Http { url } => serde_json::json!({
                "type": "http", "url": url, "headers": values
            }),
        };
        declared.insert(server.name.clone(), entry);
    }
    serde_json::to_string(&serde_json::json!({ "mcpServers": declared }))
        .map_err(|_| failure("launch_context_unavailable"))
}

/// Refuses launch context files where owner-only no-follow creation does not exist.
/// Takes its inputs, all unused. Returns `launch_context_unavailable`.
#[cfg(not(unix))]
fn lock(_home: &Path) -> Result<File, LaunchError> {
    Err(failure("launch_context_unavailable"))
}

#[cfg(unix)]
fn lock(home: &Path) -> Result<File, LaunchError> {
    let file = File::from(
        rustix::fs::open(
            home.join(LOCK),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| failure("launch_context_unsafe"))?,
    );
    let metadata = file
        .metadata()
        .map_err(|_| failure("launch_context_unsafe"))?;
    if !metadata.is_file() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(failure("launch_context_unsafe"));
    }
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| failure("launch_home_busy"))?;
    Ok(file)
}

/// Removes the Codex skill copies at `<home>/skills/crazyagent` without resolving `skills`
/// by path, because the agent may have replaced that folder with a link: the copies are
/// moved up into the verified home through descriptors and removed there. A `skills` that
/// is not a real folder of this user is left alone. Input: the validated agent home.
#[cfg(unix)]
fn remove_copies(home: &Path) {
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(parent) = rustix::fs::open(home, folder, Mode::empty()) else {
        return;
    };
    let Ok(skills) = rustix::fs::openat(&parent, "skills", folder, Mode::empty()) else {
        return;
    };
    if !rustix::fs::fstat(&skills)
        .is_ok_and(|stat| stat.st_uid == rustix::process::geteuid().as_raw())
    {
        return;
    }
    let parked = format!(".{STATE}-skills-{}", std::process::id());
    remove(&home.join(&parked));
    if rustix::fs::renameat(&skills, STATE, &parent, parked.as_str()).is_ok() {
        remove(&home.join(&parked));
    }
}

/// Leaves skill copies in place where descriptor-relative removal does not exist; no
/// launch reaches this, because the home lock is refused first. Input: unused.
#[cfg(not(unix))]
fn remove_copies(_home: &Path) {}

fn remove(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// Refuses launch context files where owner-only no-follow creation does not exist.
/// Takes its inputs, all unused. Returns `launch_context_unavailable`.
#[cfg(not(unix))]
fn directory(_path: &Path, _private: bool) -> Result<(), LaunchError> {
    Err(failure("launch_context_unavailable"))
}

#[cfg(unix)]
fn directory(path: &Path, private: bool) -> Result<(), LaunchError> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(problem) if problem.kind() == std::io::ErrorKind::AlreadyExists && !private => {}
        Err(_) => return Err(failure("launch_context_unsafe")),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| failure("launch_context_unsafe"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || (private && metadata.permissions().mode() & 0o077 != 0)
    {
        return Err(failure("launch_context_unsafe"));
    }
    Ok(())
}

#[cfg(unix)]
fn create(path: &Path) -> Result<File, LaunchError> {
    Ok(File::from(
        rustix::fs::open(
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| failure("launch_context_unsafe"))?,
    ))
}

/// Refuses launch context files where owner-only no-follow creation does not exist.
/// Takes its inputs, all unused. Returns `launch_context_unavailable`.
#[cfg(not(unix))]
fn write_private(_path: &Path, _bytes: &[u8]) -> Result<(), LaunchError> {
    Err(failure("launch_context_unavailable"))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), LaunchError> {
    let mut file = create(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("launch_context_unwritable"))?;
    assert_eq!(
        file.metadata().map(|m| m.permissions().mode() & 0o777).ok(),
        Some(0o600)
    );
    Ok(())
}

fn write_secret(path: &Path, text: String) -> Result<(), LaunchError> {
    let mut bytes = text.into_bytes();
    let written = write_private(path, &bytes);
    bytes.fill(0);
    written
}

fn copy_tree(
    source: &Path,
    target: &Path,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), LaunchError> {
    if depth > SKILL_DEPTH_MAX {
        return Err(failure("launch_context_too_large"));
    }
    if !source.is_absolute()
        || !fs::symlink_metadata(source)
            .map_err(skill_unavailable)?
            .is_dir()
    {
        return Err(failure("launch_context_skill_unavailable"));
    }
    directory(target, true)?;
    for entry in fs::read_dir(source)
        .map_err(skill_unavailable)?
        .take(SKILL_FILES_MAX + 1)
    {
        let entry = entry.map_err(skill_unavailable)?;
        let kind = entry.file_type().map_err(skill_unavailable)?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &to, depth + 1, budget)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &to, budget)?;
        }
    }
    Ok(())
}

/// Refuses launch context files where owner-only no-follow creation does not exist.
/// Takes its inputs, all unused. Returns `launch_context_unavailable`.
#[cfg(not(unix))]
fn copy_file(_source: &Path, _target: &Path, _budget: &mut Budget) -> Result<(), LaunchError> {
    Err(failure("launch_context_unavailable"))
}

#[cfg(unix)]
fn copy_file(source: &Path, target: &Path, budget: &mut Budget) -> Result<(), LaunchError> {
    let from = File::from(
        rustix::fs::open(
            source,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(skill_unavailable)?,
    );
    let metadata = from.metadata().map_err(skill_unavailable)?;
    budget.files += 1;
    budget.bytes = budget.bytes.saturating_add(metadata.len());
    if !metadata.is_file() {
        return Err(failure("launch_context_skill_unavailable"));
    }
    if budget.files > SKILL_FILES_MAX || budget.bytes > SKILL_BYTES_MAX {
        return Err(failure("launch_context_too_large"));
    }
    let mut to = create(target)?;
    let copied = std::io::copy(&mut from.take(metadata.len()), &mut to)
        .map_err(|_| failure("launch_context_unwritable"))?;
    assert!(copied <= metadata.len());
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "launch_context/tests.rs"]
mod tests;
