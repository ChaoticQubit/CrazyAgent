use crate::agent_git::Layout;
use crate::coord::settings::{ClaudeDisplay, RoleSkills, SkillRole};
use crate::harnesses::launch::{LaunchError, SecretInCommand};
use crate::plugins::{
    EffectiveHook, EffectivePlugins, EffectiveSkill, LaunchPlugins, LaunchSecret, LaunchServer,
    ServerSpec, hook_folder,
};
use crate::project_setup::{ContextKind, StoredSetup};
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction};
#[cfg(unix)]
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File};
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const HEADER_BYTES_MAX: usize = HEADER_TITLE.len()
    + "You are working on the project \"\".\n".len()
    + NAME_BYTES_MAX
    + 2 * ("Project folder: \n".len() + FOLDER_BYTES_MAX);
pub const BRIEF_BYTES_MAX: usize = 3 * 1024;
pub const INDEX_ITEMS_MAX: usize = crate::project_setup::CONTEXT_ITEMS_MAX;
pub const INDEX_BYTES_MAX: usize = 3 * 1024;
pub const MEMORY_ITEMS_MAX: usize = crate::memory::library::LAUNCH_ITEMS_MAX;
pub const MEMORY_SECTION_BYTES_MAX: usize = crate::memory::library::LAUNCH_BYTES_MAX + 1024;
pub const INSTRUCTIONS_BYTES_MAX: usize = 34 * 1024;
pub const SKILLS_INDEX_BYTES_MAX: usize = 8 * 1024;
pub const TEAM_INDEX_BYTES_MAX: usize = 4 * 1024;
pub const MASTER_INSTRUCTIONS_BYTES_MAX: usize = INSTRUCTIONS_BYTES_MAX
    + TEAM_INDEX_BYTES_MAX
    + SECTION_SLACK
    + MEMORY_TOOLS_BYTES_MAX
    + MASTER_LANDING_RULES.len();
/// The most bytes of the section that names a master's `remember` and `recall` tools.
pub const MEMORY_TOOLS_BYTES_MAX: usize = 512;

const _: () = assert!(TOOLS_SECTION_BYTES_MAX <= TEAM_INDEX_BYTES_MAX);
/// The most bytes of the team tools section a live master or sub-agent with tools reads.
pub const TOOLS_SECTION_BYTES_MAX: usize = 3 * 1024;
/// The most bytes of a live master's own instructions: its role, its team tools or the
/// DELEGATE line, and how the team's work lands.
pub const LIVE_RULES_BYTES_MAX: usize = 4 * 1024;
/// The most bytes a live master adds to what its harness reads at launch: its own
/// instructions and the seed of a fresh conversation for a session with earlier turns.
pub const LIVE_INSTRUCTIONS_BYTES_MAX: usize =
    LIVE_RULES_BYTES_MAX + 2 + crate::master_sessions::SEED_BYTES_MAX;

/// How a Sandboxed agent's work lands, given at every launch in a linked worktree:
/// CrazyAgent, not the agent, turns the edits left in its worktree into a change set the
/// owner accepts in Changes, and the sandbox refuses its commits.
pub const LANDING_RULES: &str = "\n## How your work lands\n\nCrazyAgent lands your work; you never do. Never create, switch or delete a branch, never commit, push or tag, and never write inside `.git`. If the sandbox refuses a command, never retry it outside the sandbox or another way; say in your reply what was refused. Leave your edits as files in your worktree. When your task ends, CrazyAgent turns them into a change set, and the owner accepts it in the project's Changes view onto the branch checked out in the project folder. This rule wins over any other instruction to branch or commit.\n";

/// What a master is told about landing its team's work: it never tells a sub-agent how to
/// branch or commit, since each sub-agent's own rules follow its layout, and it sends the
/// owner to Changes when change sets wait.
pub const MASTER_LANDING_RULES: &str = "\n## Landing the team's work\n\nNever tell a sub-agent how to branch, commit or push, and never name a base branch in a task: each sub-agent's own instructions say how its work becomes a change set. When change sets wait, tell the owner to review and accept them in the project's Changes view; accepting lands them on the branch checked out in the project folder.\n";
/// How an Own branch or Full access agent's work lands, given at every launch in its own
/// clone: it commits on its branch, and the owner's Accept lands only those commits.
pub const OWN_BRANCH_LANDING_RULES: &str = "\n## How your work lands\n\nYour worktree is your own copy of the project, on its own branch. Commit your work on the branch checked out in your worktree with conventional commit messages before your task ends: only your commits land, and uncommitted edits stay behind. Never create, switch, rename or delete a branch, never push, tag or rewrite history, and never write inside the project folder's `.git`. If the sandbox refuses a command, never retry it outside the sandbox or another way; say in your reply what was refused. When your task ends, CrazyAgent fetches your branch, and the owner's Accept in the project's Changes view lands your commits onto the branch checked out in the project folder. This rule wins over any other instruction about branches or commits.\n";

pub const LANDING_RULES_BYTES_MAX: usize = if LANDING_RULES.len() > OWN_BRANCH_LANDING_RULES.len() {
    LANDING_RULES.len()
} else {
    OWN_BRANCH_LANDING_RULES.len()
};
pub const SKILL_FILES_MAX: usize = 4096;
pub const SKILL_BYTES_MAX: u64 = 64 * 1024 * 1024;
const SKILL_DEPTH_MAX: usize = 8;
const SECTION_SLACK: usize = 512;
const ASK_RULE_BYTES_MAX: usize = 1024;
pub(crate) const NAME_BYTES_MAX: usize = 256;
const DESCRIPTION_BYTES_MAX: usize = 512;
const SKILL_LINE_DESCRIPTION_BYTES_MAX: usize = 160;
const SKILL_TOOL_USE: &str = "Use one with the Skill tool, where it is named crazyagent:NAME; without a Skill tool, read its SKILL.md at the path shown.";
const SKILL_FILE_USE: &str = "To use one, read its SKILL.md at the path shown and follow it.";
const FOLDER_BYTES_MAX: usize = 1024;
const HEADER_TITLE: &str = "# Project context from CrazyAgent\n\n";
pub(crate) const STATE: &str = "crazyagent";
/// The folder under a Codex home's `skills` where Codex 0.164.0 installs its own system
/// skills at each start.
pub(crate) const CODEX_SYSTEM_SKILLS: &str = ".system";
const CONTEXT_MODE_SERVER: &str = "context-mode";
const CONTEXT_MODE_DECLARED: &str = "plugin_context-mode_context-mode";
/// The context-mode tools the "Tools to use" section names, in the order it names them.
const CONTEXT_MODE_TOOLS: [&str; 5] = [
    "ctx_batch_execute",
    "ctx_execute",
    "ctx_execute_file",
    "ctx_fetch_and_index",
    "ctx_search",
];
/// The most bytes of the section that tells a tools-on launch when to use context-mode,
/// graphify and its skills.
pub const TOOL_USE_BYTES_MAX: usize = 2 * 1024;
/// The most bytes of the Task-start reminder the first turn of a task carries (T3-15).
pub const TASK_START_BYTES_MAX: usize = 1024;
const TEAMMATE_COMMAND: &str = "teammate";
const TEAMMATE_INSTRUCTIONS: &str = "teammate.md";
const MATES_HOME: &str = "mates-home";
/// The most switched-off tools one launch passes to its harness, across every server.
pub const DENIED_TOOLS_MAX: usize = 256;
#[cfg(unix)]
const LOCK: &str = "crazyagent.lock";
const PLUGIN_MANIFEST: &str = "{\"name\":\"crazyagent\",\"version\":\"1.0.0\",\"description\":\"Skills the owner approved for this project in CrazyAgent\"}\n";
const HOOK_PLUGINS: &str = "hook-plugins";
const HOOK_ROOT: &str = "CRAZYAGENT_HOOK_ROOT";
const PLUGIN_ROOT: &str = "${CLAUDE_PLUGIN_ROOT}";

const _: () = assert!(
    HEADER_BYTES_MAX
        + LANDING_RULES_BYTES_MAX
        + BRIEF_BYTES_MAX
        + INDEX_BYTES_MAX
        + MEMORY_SECTION_BYTES_MAX
        + 3 * SECTION_SLACK
        <= INSTRUCTIONS_BYTES_MAX
);
const _: () = assert!(
    MASTER_INSTRUCTIONS_BYTES_MAX <= crate::adapters::codex::DEVELOPER_INSTRUCTIONS_BYTES_MAX
);
const _: () = assert!(INSTRUCTIONS_BYTES_MAX + ASK_RULE_BYTES_MAX <= MASTER_INSTRUCTIONS_BYTES_MAX);

fn failure(code: &str) -> LaunchError {
    LaunchError(code.into(), None)
}

/// The owner's sentence for a launch refused because its command would hold an MCP secret:
/// names the server and the secret, never the value, and where to replace it. Input: the
/// refusal. Output: the sentence, or None for any other refusal.
pub fn secret_in_command_text(problem: &LaunchError) -> Option<String> {
    let named = problem.1.as_deref()?;
    assert_eq!(problem.0, "launch_secret_in_command");
    Some(format!(
        "The MCP server \"{}\" secret \"{}\" is part of the launch command, so it cannot be \
         kept hidden. Replace it in Plugins.",
        named.server, named.secret
    ))
}

fn unavailable<E>(_: E) -> LaunchError {
    failure("launch_context_unavailable")
}

fn skill_unavailable<E>(_: E) -> LaunchError {
    failure("launch_context_skill_unavailable")
}

/// What every agent the engine launches for one project receives: the approved skills and
/// MCP servers with their secret values, the tools the owner switched off on each server,
/// one bounded startup instruction holding the project brief, an index of its context files
/// and the owner-approved memories, whether a headless Claude run keeps its transcript,
/// whether a Claude Code lead runs its own agent team, the engine executable and mailbox
/// of a team agent's message relay, the loopback port of the model proxy a team agent
/// under the None network reaches its model through (S4b), the display its Claude Code
/// terminal draws with (N-D9) and whether its team tools section tells it to merge
/// `crazyagent/main` ("Catch up with main", OS-8).
#[derive(Debug)]
pub struct AgentContext {
    plugins: LaunchPlugins,
    instructions: String,
    process: Option<Box<Process>>,
    transcripts: bool,
    off_tools: Vec<(String, Vec<String>)>,
    native_team: Option<(PathBuf, PathBuf)>,
    relay: Option<(PathBuf, PathBuf)>,
    model_proxy: Option<u16>,
    claude_display: ClaudeDisplay,
    catch_up_main: bool,
}

/// The project whose app and project instructions and instruction files each launch reads
/// and writes into the agent's home, the hooks the owner turned on for it, and the owner's
/// own instructions for the one team agent launched, empty for none.
#[derive(Debug)]
struct Process {
    project: crate::agent_home::AgentProject,
    hooks: Vec<EffectiveHook>,
    agent: String,
}

/// The files one launch wrote into its agent home, whether a headless Claude run keeps its
/// transcript, whether its generated plugin holds hooks, the Claude Code tool names the
/// owner switched off, whether the lead runs its own agent team, the model proxy port its
/// variables name, and the lock that keeps a second launch out of that home. Dropping it deletes every file holding a secret and the
/// copied skills and hooks, unless `keep` left them to a pane that still runs.
#[derive(Debug)]
pub struct Installed {
    arguments: Vec<OsString>,
    files: Option<InstalledFiles>,
    transcripts: bool,
    hooks: bool,
    relay: bool,
    denied: Vec<String>,
    native: Option<NativeTeam>,
    model_proxy: Option<u16>,
    claude_display: ClaudeDisplay,
    _lock: File,
}

/// What a native-team lead's launch gives its teammates (DEC-104 K5a): the tmux program
/// Claude Code splits their panes with, which is on the lead's PATH, and the instruction
/// file each teammate starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTeam {
    pub tmux: PathBuf,
    pub instructions: PathBuf,
}

/// Where one launch wrote its files, as a pane record keeps it, so an engine that adopts
/// the pane later deletes them when the pane stops: the agent home, and whether the launch
/// was for Codex, which also writes `config.toml` and the skill copies there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledFiles {
    pub home: PathBuf,
    pub codex: bool,
}

struct Budget {
    files: usize,
    bytes: u64,
}

/// Reads what agents launched for one project receive, in one read transaction.
/// Input: a connection to the engine's `engine.db` (the engine root is its folder), the
/// project and the agent's git layout, which picks how its work lands. Output: the context,
/// or a plain code such as `plugin_project_unknown`.
pub fn for_project(
    connection: &Connection,
    project: &ProjectId,
    layout: Layout,
) -> Result<AgentContext, LaunchError> {
    let root = engine_root(connection)?;
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    let plugins = crate::storage::plugins::launch_set(&tx, &root, project)
        .map_err(|problem| LaunchError(problem.0, None))?;
    let instructions = project_instructions(&tx, project, layout)?;
    let home_project = agent_project(&tx, project)?;
    let transcripts = crate::instruction_files::load(&tx, project)
        .map_err(failure)?
        .transcripts;
    let hooks = crate::storage::plugins::effective_hooks(&tx, &root, project)
        .map_err(|problem| LaunchError(problem.0, None))?;
    let off_tools = off_tools(&tx, project, &plugins)?;
    let catch_up_main = crate::team::catch_up_main(&tx, project)
        .map_err(|problem| LaunchError(problem.0, None))?
        == crate::team::CatchUpMain::On;
    drop(tx);
    crate::agent_home::text(&home_project).map_err(|problem| LaunchError(problem.0, None))?;
    Ok(AgentContext {
        plugins,
        instructions,
        process: Some(Box::new(Process {
            project: home_project,
            hooks,
            agent: String::new(),
        })),
        transcripts,
        off_tools,
        native_team: None,
        relay: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
        catch_up_main,
    })
}

/// Reads what one team role's agents launched for a project receive (DEC-111 R3): the
/// project's context with only the approved skills the owner chose for that role. Inputs: a
/// connection to the engine's `engine.db`, the project, the agent's git layout and its
/// role. Output: the context, or a plain code.
pub fn for_team_role(
    connection: &Connection,
    project: &ProjectId,
    layout: Layout,
    role: SkillRole,
) -> Result<AgentContext, LaunchError> {
    let chosen = crate::coord::settings::read(connection, project)
        .map_err(|problem| LaunchError(problem.0, None))?
        .role_skills;
    Ok(for_project(connection, project, layout)?.for_role(chosen.of(role)))
}

/// Reads the tools the owner switched off, for the project or app-wide, on each approved MCP
/// server and on the engine's own server, by the server's stored name. Input: the read
/// transaction, the project and its approved plugins. Output: each server with switched-off
/// tools and their names, or `launch_context_tool_switch_invalid` for a stored name that is
/// not a plain tool name or more than `DENIED_TOOLS_MAX` names in all.
fn off_tools(
    tx: &Transaction<'_>,
    project: &ProjectId,
    plugins: &LaunchPlugins,
) -> Result<Vec<(String, Vec<String>)>, LaunchError> {
    let invalid = || failure("launch_context_tool_switch_invalid");
    let mut found = Vec::new();
    let mut total = 0;
    let names = plugins
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .chain([crate::coord::tool_switches::CRAZYAGENT_SERVER]);
    for server in names {
        let off = crate::coord::tool_switches::off_tools(tx, project, server)
            .map_err(|problem| LaunchError(problem.0, None))?;
        if off.is_empty() || found.iter().any(|(seen, _): &(String, _)| seen == server) {
            continue;
        }
        if off.iter().any(|tool| !tool_name_plain(tool)) {
            return Err(invalid());
        }
        total += off.len();
        if total > DENIED_TOOLS_MAX {
            return Err(invalid());
        }
        found.push((server.to_owned(), off));
    }
    assert!(found.iter().map(|(_, off)| off.len()).sum::<usize>() <= DENIED_TOOLS_MAX);
    Ok(found)
}

fn tool_name_plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::coord::tool_switches::NAME_BYTES_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// The part only a native lead's teammates read after the lead's own instructions: they work
/// in the lead's agent team, and graphify finds the project graph through the lead's
/// worktree, since its read-only graph links exist only there. Input: the lead's worktree.
/// Output: the section, or `launch_path_invalid` for a path with a control character.
fn teammate_section(worktree: &Path) -> Result<String, LaunchError> {
    let folder = worktree
        .to_str()
        .filter(|text| !text.chars().any(|c| c.is_control() || c == '`'))
        .ok_or_else(|| failure("launch_path_invalid"))?;
    Ok(format!(
        "\n## You are a teammate\n\nYou are a teammate in the Claude Code agent team of a CrazyAgent lead, which works in `{folder}`. Your own worktree has no project graph: run graphify with `--graph {folder}/graphify-out/graph.json`.\n"
    ))
}

/// Where a native-team lead's launch writes the command Claude Code runs for each teammate
/// (`CLAUDE_CODE_TEAMMATE_COMMAND`), inside the launch's own files so it goes when they do.
/// Input: the agent home. Output: the path.
pub(crate) fn teammate_command(home: &Path) -> PathBuf {
    home.join(STATE).join(TEAMMATE_COMMAND)
}

/// The config home a native lead's teammates run with as their `CLAUDE_CONFIG_DIR` and
/// `HOME`, beside the lead's own, so that no teammate writes a file the lead's Claude Code
/// or shell reads as code (TPH-2). Input: the lead's agent home. Output: the path.
pub(crate) fn mates_home(home: &Path) -> PathBuf {
    home.with_file_name(MATES_HOME)
}

/// Names what an agent home of the project receives the owner's instructions from: the
/// engine data folder (the folder of `engine.db`), the project's recorded folder and which
/// of the owner's two instruction texts its agents get.
/// Input: a transaction on `engine.db` and the project. Output: the project, or a plain code.
pub(crate) fn agent_project(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<crate::agent_home::AgentProject, LaunchError> {
    let folder = crate::project_setup::load_setup(tx, project)
        .map_err(unavailable)?
        .map(|setup| setup.project_folder())
        .filter(|folder| folder.is_absolute());
    let mode = crate::instruction_files::load(tx, project)
        .map_err(failure)?
        .mode;
    Ok(crate::agent_home::AgentProject {
        data: engine_root(tx)?,
        id: project.clone(),
        folder,
        mode,
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

/// Builds the startup instruction for the engine's own project master conversation: the same
/// project context, approved memories and linked-worktree landing rules every Sandboxed agent gets,
/// since the conversation runs in a linked worktree, then with tools on what its tools are, how it
/// starts a sub-agent (DEC-093) and its memory tools less the ones switched off, as a team
/// master's, or with tools off the names of the approved skills and MCP servers that team agents
/// with tools on receive, with the team roles each skill goes to when the owner chose skills per
/// role, then how the team's work lands. No secret value, command, URL or file of a server is read
/// or given. Input: a transaction the caller holds on `engine.db`, the project, with tools on the
/// team tools switched off, none with tools off, and the live model access, none without settings.
/// Output: at most `MASTER_INSTRUCTIONS_BYTES_MAX` bytes of markdown, or a plain code.
pub fn for_master(
    tx: &Transaction<'_>,
    project: &ProjectId,
    tools: Option<&[String]>,
    access: Option<&crate::config::ModelAccess>,
) -> Result<String, LaunchError> {
    let root = engine_root(tx)?;
    let mut text = project_instructions(tx, project, Layout::Linked)?;
    if let Some(off) = tools {
        let setup = crate::project_setup::load_setup(tx, project)
            .map_err(unavailable)?
            .ok_or_else(|| failure("launch_context_master_tools_without_plan"))?;
        let available = access
            .map(|access| crate::team::available_harnesses(access, project))
            .unwrap_or_default();
        text.push_str(&master_tools_section(&setup, &available));
        text.push_str(&memory_tools_section(off));
    } else {
        let plugins = crate::storage::plugins::effective(tx, &root, project)
            .map_err(|problem| LaunchError(problem.0, None))?;
        let roles = crate::coord::settings::read(tx, project)
            .map_err(|problem| LaunchError(problem.0, None))?
            .role_skills;
        text.push_str(&team_index(&plugins, &roles));
    }
    text.push_str(MASTER_LANDING_RULES);
    assert!(text.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
    Ok(text)
}

/// Tells a conversation master whose tools are on what it can do and how it starts a
/// sub-agent: its worktree is the only folder it writes, its `delegate` tool hands one task
/// to the team, its `ask` tool is the only way it asks the owner, and the team plan's
/// sub-agents, or for an empty plan the harnesses it can use.
/// Input: the project setup and the harnesses with an eligible account now.
/// Output: at most `TEAM_INDEX_BYTES_MAX` plus slack bytes.
pub(crate) fn master_tools_section(setup: &StoredSetup, available: &[&str]) -> String {
    let mut section = format!(
        "\n## Your tools\n\nYour tools are on in this conversation. You can read and search files, edit them and run shell commands in this conversation's own git worktree, your working folder. You cannot write outside it; the project folder itself stays read-only to you.\n\nTo start a sub-agent, call the `{}` tool of the `{}` MCP server with the task in plain words and, if you choose, a harness from the team plan. Each call hands over one task, at most {} in one reply. Start sub-agents only with this tool. Never use your harness's own sub-agent tools, such as Codex's `spawn_agent`, `followup_task`, `send_message` or `wait_agent`: CrazyAgent stops any agent they start, and only agents started with this tool run on the project's pool and appear in the project's Agents tab and Team view. When your reply ends, CrazyAgent checks each task against the team plan and runs it in its own git worktree. Its full progress and result show in the project's Team view. When it ends, a short note on how it went comes back to this conversation as an `Engine:` line you see on your next turn.\n\n",
        crate::team::tool::TOOL_NAME,
        crate::team::tool::SERVER_NAME,
        crate::team::DELEGATIONS_PER_REPLY_MAX
    );
    let rule = ask_rule(AskReach::ReplyEnd, false);
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
            if left > 0 || section.len() + line.len() + rule.len() > TEAM_INDEX_BYTES_MAX {
                left += 1;
                continue;
            }
            section.push_str(&line);
        }
        if left > 0 {
            section.push_str(&format!("- and {left} more\n"));
        }
    }
    section.push_str(&rule);
    assert!(section.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK);
    section
}

/// The section that names a live master's or sub-agent's team tools (DEC-104), each on
/// one line, how to restart from main after an accepted change set, the two question tags,
/// the branch rule for the agents it leads (Q-W-LAND-26) and, while "Catch up with main" is
/// on, how to bring a moved main into its own branch (OS-8). Inputs: the team tools left out
/// and the setting. Output: at most `TOOLS_SECTION_BYTES_MAX` bytes of markdown.
pub fn team_tools_section(off: &[String], catch_up_main: bool) -> String {
    let mut section = format!(
        "\n## Team tools\n\nYour team tools are in the `{}` MCP server. The engine answers each by your place in the team:\n",
        crate::team::tool::SERVER_NAME
    );
    for (name, line) in crate::coord::tool_switches::BUILTIN_TOOLS {
        if !off.iter().any(|tool| tool == name) {
            section.push_str(&format!("- {name}: {line}\n"));
        }
    }
    section.push_str("Call start_work with your claims before you edit, read inspect_work when the engine says something waits for you, and end with finish_work and its evidence. After your change set is accepted, start your next work from main with git reset --hard crazyagent/main, unless you have newer commits.\nEvery question carries two tags: for (me for the owner, master, or lead for your own lead) and severity (high, normal or low). A for me question reaches the owner only; no agent answers it. Pass a question you cannot answer on up with update_work, never at a lower severity.\nBranches: an agent you lead works on `<your task branch>--<its name>`, never `<your task branch>/<its name>`, which git refuses.\n");
    if catch_up_main {
        section.push_str("When main has moved on, bring it into your branch with git merge crazyagent/main before you finish.\n");
    }
    assert!(section.len() <= TOOLS_SECTION_BYTES_MAX);
    section
}

/// Renders the section that names the memory tools of a master running without its live terminal, a
/// team master or a console turn, less the ones switched off, and says that only they keep what
/// later sessions and the owner see. Takes the tools left out. Returns the markdown, empty when
/// both are off.
fn memory_tools_section(off: &[String]) -> String {
    let lines: String = crate::coord::tool_switches::BUILTIN_TOOLS
        .iter()
        .filter(|(name, _)| {
            crate::team::tool::MEMORY_TOOLS.contains(name) && !off.iter().any(|tool| tool == name)
        })
        .map(|(name, line)| format!("- {name}: {line}\n"))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let section = format!(
        "\n## Memory tools\n\nYour memory tools are in the `{}` MCP server. Keep what later sessions should know with remember: the owner sees it in Project memory, and memory you keep anywhere else is never seen there:\n{lines}",
        crate::team::tool::SERVER_NAME
    );
    assert!(section.len() <= MEMORY_TOOLS_BYTES_MAX);
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

fn project_instructions(
    tx: &Transaction<'_>,
    project: &ProjectId,
    layout: Layout,
) -> Result<String, LaunchError> {
    let setup = crate::project_setup::load_setup(tx, project).map_err(unavailable)?;
    let memories = crate::memory::library::approved_for_project(tx, project)
        .map_err(unavailable)?
        .render();
    let instructions = render(project, setup.as_ref(), &memories, layout);
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

    /// Builds the Task-start reminder the first turn of each task carries while the setting
    /// is On (T3-15): for Claude Code the ToolSearch select string of the context-mode tools
    /// left on, the one its "Tools to use" section gives, and the approved skills by their
    /// Skill tool names; for Codex those tools on their MCP server and the skills' SKILL.md.
    /// Skills past the bound are named as the rest of the Skills section. Inputs: the harness
    /// id and the launch's tools switch. Output: at most `TASK_START_BYTES_MAX` bytes, or None
    /// with tools off, for another harness, or with no context-mode tool and no skill to name.
    pub fn task_start(&self, harness: &str, tools: bool) -> Option<String> {
        const MORE: &str = ", and the other skills in the Skills section";
        let id = crate::harnesses::spec(harness)
            .map(|spec| spec.id)
            .filter(|id| matches!(*id, "codex" | "claude-code"))?;
        let claude = id == "claude-code";
        let names = context_mode_names(claude, &self.off_tools);
        let context_mode = !names.is_empty()
            && self
                .plugins
                .servers
                .iter()
                .any(|server| server.name == CONTEXT_MODE_SERVER);
        if !tools || (!context_mode && self.plugins.skills.is_empty()) {
            return None;
        }
        let mut text = String::from("Task start reminder: before you work on this task, ");
        if context_mode {
            text.push_str(&match claude {
                true => format!(
                    "run ToolSearch with `select:{}` and use those context-mode tools",
                    names.join(",")
                ),
                false => format!(
                    "use the `{}` tools of the `{CONTEXT_MODE_DECLARED}` MCP server",
                    names.join("`, `")
                ),
            });
            text.push_str(
                " for any command, test, search or file read whose output may pass 20 lines",
            );
        }
        if !self.plugins.skills.is_empty() {
            if context_mode {
                text.push_str(", and ");
            }
            text.push_str(match claude {
                true => "call the Skill tool for each of these skills that fits this task: ",
                false => {
                    "read the SKILL.md of each of these skills that fits this task and follow it: "
                }
            });
            let room = TASK_START_BYTES_MAX.saturating_sub(text.len() + MORE.len() + 1);
            let mut listed = String::new();
            for skill in &self.plugins.skills {
                let name = match claude {
                    true => format!("`crazyagent:{}`", skill.name),
                    false => format!("`{}`", skill.name),
                };
                let gap = if listed.is_empty() { "" } else { ", " };
                if listed.len() + gap.len() + name.len() > room {
                    listed.push_str(MORE);
                    break;
                }
                listed.push_str(gap);
                listed.push_str(&name);
            }
            text.push_str(&listed);
        }
        text.push('.');
        assert!(text.len() <= TASK_START_BYTES_MAX);
        Some(text)
    }

    /// Keeps only the approved skills the owner chose for one team role (DEC-111 R3); a
    /// chosen name that is not approved adds nothing. Input: the role's chosen names, or
    /// none for All approved skills, which keeps every one. Output: the context.
    pub fn for_role(mut self, chosen: Option<&[String]>) -> Self {
        if let Some(names) = chosen {
            let before = self.plugins.skills.len();
            self.plugins
                .skills
                .retain(|skill| names.contains(&skill.name));
            assert!(self.plugins.skills.len() <= before.min(names.len()));
        }
        self
    }

    /// Whether this launch's project has "Catch up with main" on, as read once with the rest
    /// of the context, so the agent's own `git` copy and its team tools section agree.
    /// Output: the choice.
    pub fn catch_up_main(&self) -> bool {
        self.catch_up_main
    }

    /// Lists what a project sandbox must let an agent with tools on read beyond its home, as
    /// paths whose folders it reads: the executable of every approved command server and,
    /// for context-mode run from the engine's shared install, that install's `node_modules`.
    pub fn read_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(2 * self.plugins.servers.len());
        for server in &self.plugins.servers {
            if let ServerSpec::Stdio { command, .. } = &server.spec {
                paths.push(PathBuf::from(command));
            }
            #[cfg(unix)]
            paths.extend(
                crate::context_mode::shared_install(&server.spec)
                    .map(|install| install.join("node_modules")),
            );
        }
        paths
    }

    /// Adds a live master's own instructions to what its harness reads at launch, so they
    /// reach it as system context and never as a typed message. Input: at most
    /// `LIVE_INSTRUCTIONS_BYTES_MAX` bytes the engine built. Output: the context.
    pub fn with_live_instructions(mut self, text: &str) -> Self {
        assert!(text.len() <= LIVE_INSTRUCTIONS_BYTES_MAX);
        self.instructions.push('\n');
        self.instructions.push_str(text);
        assert!(
            self.instructions.len()
                <= MASTER_INSTRUCTIONS_BYTES_MAX + 1 + LIVE_INSTRUCTIONS_BYTES_MAX
        );
        self
    }

    /// Lets a Claude Code lead with tools on run its own agent team (DEC-104 Q10, native mode):
    /// its launch keeps Agent and TeamCreate, starts teammates in tmux panes of its own
    /// terminal and gives each the lead's instructions and where the project graph is.
    /// Other harnesses and tools-off launches ignore it. Input: the tmux program the lead's
    /// own server runs and the lead's worktree. Output: the context.
    pub fn with_native_team(mut self, tmux: &Path, worktree: &Path) -> Self {
        assert!(tmux.is_absolute() && worktree.is_absolute());
        self.native_team = Some((tmux.to_path_buf(), worktree.to_path_buf()));
        self
    }

    /// Sends a team agent's model traffic through its model proxy (S4b): its launch gets
    /// `HTTPS_PROXY` and `HTTP_PROXY` naming the proxy on 127.0.0.1 and an empty `NO_PROXY`,
    /// and its pane record keeps the port for a later engine. Input: the proxy's port.
    /// Output: the context.
    pub fn with_model_proxy(mut self, port: u16) -> Self {
        assert_ne!(port, 0);
        self.model_proxy = Some(port);
        self
    }

    /// Gives this launch's Claude Code terminal the project's display (N-D9): Classic turns
    /// Claude Code's alternate screen off, Fullscreen keeps Claude Code's own view. Other
    /// harnesses and headless runs ignore it. Input: the display. Output: the context.
    pub fn with_claude_display(mut self, display: ClaudeDisplay) -> Self {
        self.claude_display = display;
        self
    }

    /// Gives a team agent's Claude Code launch the message relay plugin (DEC-104 K2a), which
    /// delivers its team messages and reports its activity through its mailbox, and which its
    /// native teammates get from it. Other harnesses ignore it. Input: the absolute engine
    /// executable the relay runs and the agent's absolute mailbox. Output: the context.
    pub fn with_relay(mut self, engine: &Path, mailbox: &Path) -> Self {
        self.relay = Some((engine.to_path_buf(), mailbox.to_path_buf()));
        self
    }

    /// Adds the owner's own instructions for one team agent after the app and project text
    /// this context writes into its home, before the instruction files section, which stays
    /// last. Inputs: a connection to `engine.db`, the project and the agent id, above 0.
    /// Output: the context, or `launch_context_unavailable`.
    pub fn for_agent(
        mut self,
        connection: &Connection,
        project: &ProjectId,
        agent: i64,
    ) -> Result<Self, LaunchError> {
        let tx = connection.unchecked_transaction().map_err(unavailable)?;
        let (text, _) =
            crate::storage::agent_text::read(&tx, project, agent).map_err(unavailable)?;
        drop(tx);
        assert!(text.len() <= crate::agent_home::TEXT_BYTES_MAX);
        if let Some(process) = self.process.as_mut() {
            process.agent = text;
        }
        Ok(self)
    }

    /// Adds the engine's own MCP server that gives a team master with tools on, running
    /// without its live terminal, its `delegate` and `ask` tools, both taken when its reply
    /// ends, and its `remember` and `recall` tools, answered by the engine through the
    /// mailbox folder, and the section that names those two, and tells it to ask the owner
    /// only with `ask`. Input: the absolute canonical engine executable that serves it, the
    /// absolute drop file and mailbox in the master's private temporary folder and the team
    /// tools left out. Output: the context, or a plain code when a path is not plain or an
    /// approved server already uses the name.
    pub fn with_team_tool(
        self,
        executable: &Path,
        drop: &Path,
        mailbox: &Path,
        off: &[String],
    ) -> Result<Self, LaunchError> {
        self.with_team_server(
            executable,
            crate::team::tool::SERVER_COMMAND,
            &[drop, mailbox],
            off,
            AskReach::ReplyEnd,
        )
        .map(|context| context.with_memory_section(off))
    }

    /// Adds the engine's own MCP server that gives a live team master with tools on its team
    /// tools (DEC-104), answered by the engine at once through the mailbox folder, the
    /// section that names them, and tells it to ask the owner with that server's `ask` tool
    /// or a for me `update_work` question. Input: the absolute canonical engine executable,
    /// the absolute mailbox in the master's private temporary folder and the team tools left
    /// out. Output: the context, or a plain code as for [`Self::with_team_tool`].
    pub fn with_live_team_tool(
        self,
        executable: &Path,
        mailbox: &Path,
        off: &[String],
    ) -> Result<Self, LaunchError> {
        self.with_team_server(
            executable,
            crate::team::tool::LIVE_SERVER_COMMAND,
            &[mailbox],
            off,
            AskReach::Live,
        )
        .map(|context| context.with_tools_section(off))
    }

    /// Adds the engine's own MCP server that gives a sub-agent with tools its team tools
    /// (DEC-104) and the `ask` tool, answered by the engine through the mailbox folder, the
    /// section that names them, and tells it to ask the owner with `ask` or a for me
    /// `update_work` question. Input: the absolute canonical engine executable, the absolute
    /// mailbox in the agent's private temporary folder and the tools left out. Output: the
    /// context, or a plain code as for [`Self::with_team_tool`].
    pub fn with_agent_team_tool(
        self,
        executable: &Path,
        mailbox: &Path,
        off: &[String],
        _own_clone: bool,
    ) -> Result<Self, LaunchError> {
        self.with_team_server(
            executable,
            crate::team::tool::AGENT_SERVER_COMMAND,
            &[mailbox],
            off,
            AskReach::SubAgent,
        )
        .map(|context| context.with_tools_section(off))
    }

    fn with_tools_section(mut self, off: &[String]) -> Self {
        self.instructions
            .push_str(&team_tools_section(off, self.catch_up_main));
        assert!(self.instructions.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
        self
    }

    fn with_memory_section(mut self, off: &[String]) -> Self {
        self.instructions.push_str(&memory_tools_section(off));
        assert!(self.instructions.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
        self
    }

    fn with_team_server(
        mut self,
        executable: &Path,
        subcommand: &str,
        folders: &[&Path],
        off: &[String],
        reach: AskReach,
    ) -> Result<Self, LaunchError> {
        if !crate::team::tool::known_tools(off) {
            return Err(failure("launch_context_team_tool_invalid"));
        }
        let plain = |path: &Path| {
            path.is_absolute()
                .then(|| path.to_str())
                .flatten()
                .filter(|text| !text.chars().any(char::is_control))
                .map(str::to_owned)
        };
        let (Some(command), Some(folders)) = (
            plain(executable),
            folders
                .iter()
                .copied()
                .map(&plain)
                .collect::<Option<Vec<String>>>(),
        ) else {
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
                args: std::iter::once(subcommand.to_owned())
                    .chain(folders)
                    .chain(off.iter().cloned())
                    .collect(),
            },
            secrets: Vec::new(),
        });
        if !off.iter().any(|tool| tool == crate::team::tool::ASK_TOOL) {
            let for_me = reach != AskReach::ReplyEnd
                && !off
                    .iter()
                    .any(|tool| tool == crate::team::tool::UPDATE_WORK_TOOL);
            self.instructions.push_str(&ask_rule(reach, for_me));
        }
        assert!(self.instructions.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
        Ok(self)
    }

    /// Adds the names of the project's approved skills and MCP servers for a team master
    /// whose own tools are off, with the team roles each skill goes to when the owner chose
    /// skills per role, so it plans the team's work with them. No secret value,
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
            .map_err(|problem| LaunchError(problem.0, None))?;
        let roles = crate::coord::settings::read(&tx, project)
            .map_err(|problem| LaunchError(problem.0, None))?
            .role_skills;
        drop(tx);
        Ok(self.with_index(&plugins, &roles))
    }

    fn with_index(mut self, plugins: &EffectivePlugins, roles: &RoleSkills) -> Self {
        self.instructions.push_str(&team_index(plugins, roles));
        assert!(self.instructions.len() <= MASTER_INSTRUCTIONS_BYTES_MAX);
        self
    }

    /// Reports whether any MCP secret value occurs in `text`, so launchers can prove a
    /// command line or environment never carries one.
    pub fn exposes_secret(&self, text: &[u8]) -> bool {
        self.exposed(text).is_some()
    }

    fn exposed(&self, text: &[u8]) -> Option<SecretInCommand> {
        self.plugins.servers.iter().find_map(|server| {
            server
                .secrets
                .iter()
                .find(|secret| {
                    let value = secret.value.expose();
                    !value.is_empty() && text.windows(value.len()).any(|window| window == value)
                })
                .map(|secret| SecretInCommand {
                    server: server.name.clone(),
                    secret: secret.name.clone(),
                })
        })
    }

    /// Refuses a launch whose command line or environment would hold an MCP secret of this
    /// context, as when the owner saves a secret that is also part of a path or flag the
    /// launch uses: that is the owner's input, not an engine fault. Input: every argument,
    /// environment name and value of the command. Output: Ok, or `launch_secret_in_command`
    /// naming the server and the secret.
    pub fn refuse_exposed<'a>(
        &self,
        parts: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<(), LaunchError> {
        match parts.into_iter().find_map(|part| self.exposed(part)) {
            Some(named) => Err(LaunchError(
                "launch_secret_in_command".into(),
                Some(Box::new(named)),
            )),
            None => Ok(()),
        }
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
            files: Some(InstalledFiles {
                home: home.to_path_buf(),
                codex: id == "codex",
            }),
            transcripts: self.transcripts,
            hooks: false,
            relay: false,
            denied: Vec::new(),
            native: None,
            model_proxy: self.model_proxy,
            claude_display: self.claude_display,
            _lock: lock,
        };
        remove(&state);
        directory(&state, true)?;
        let given = match &self.process {
            Some(process) => {
                let harness = crate::agent_home::Harness::from_id(id)
                    .ok_or_else(|| failure("launch_harness_unsupported"))?;
                let mut given = crate::agent_home::text(&process.project)
                    .map_err(|problem| LaunchError(problem.0, None))?;
                assert!(given.section_bytes <= given.text.len());
                let section = given.text.split_off(given.text.len() - given.section_bytes);
                given.text =
                    crate::agent_home::with_agent(std::mem::take(&mut given.text), &process.agent);
                given.text.push_str(&section);
                assert!(given.text.len() <= crate::agent_home::PROCESS_BYTES_MAX);
                crate::agent_home::prepare(home, harness, &process.project, &given)
                    .map_err(|problem| LaunchError(problem.0, None))?;
                given
            }
            None => crate::agent_home::Given::default(),
        };
        let servers: &[LaunchServer] = if tools { &self.plugins.servers } else { &[] };
        let hooks: &[EffectiveHook] = match &self.process {
            Some(process) if tools => &process.hooks,
            _ => &[],
        };
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
        if let Some(process) = &self.process {
            crate::agent_home::record_skills(&process.project.data, home, &names)
                .map_err(|problem| LaunchError(problem.0, None))?;
        }
        let first_skill = names.first().copied();
        assert_eq!(first_skill.is_some(), skills);
        if id == "codex" {
            let config = home.join("config.toml");
            let copies = home.join("skills").join(STATE);
            remove_copies(home);
            let mut developer = self.instructions.clone();
            if tools {
                developer.push_str(&tool_use_section(
                    false,
                    servers,
                    &self.off_tools,
                    first_skill,
                ));
            }
            if skills {
                directory(&home.join("skills"), false)?;
                self.copy_skills(&copies)?;
                developer.push_str(&skills_index(&copies, &self.plugins.skills, false));
            }
            assert!(
                developer.len()
                    <= MASTER_INSTRUCTIONS_BYTES_MAX
                        + 1
                        + LIVE_INSTRUCTIONS_BYTES_MAX
                        + TOOL_USE_BYTES_MAX
                        + SKILLS_INDEX_BYTES_MAX
                        + SECTION_SLACK
            );
            remove(&config);
            write_secret(&config, codex_config(&developer, servers, &self.off_tools)?)?;
        } else {
            installed.denied = denied_tools(servers, &self.off_tools);
            let mcp = state.join("mcp.json");
            write_secret(&mcp, claude_config(servers)?)?;
            installed
                .arguments
                .extend([OsString::from("--mcp-config"), mcp.into_os_string()]);
            let mut text = self.instructions.clone();
            if !given.text.is_empty() {
                text.push('\n');
                text.push_str(&given.text);
            }
            if tools {
                text.push_str(&tool_use_section(
                    true,
                    servers,
                    &self.off_tools,
                    first_skill,
                ));
            }
            if skills || !hooks.is_empty() {
                let plugin = state.join("plugin");
                directory(&plugin, true)?;
                directory(&plugin.join(".claude-plugin"), true)?;
                write_private(
                    &plugin.join(".claude-plugin").join("plugin.json"),
                    PLUGIN_MANIFEST.as_bytes(),
                )?;
                if skills {
                    self.copy_skills(&plugin.join("skills"))?;
                    text.push_str(&skills_index(
                        &plugin.join("skills"),
                        &self.plugins.skills,
                        true,
                    ));
                }
                installed.hooks = write_hooks(&plugin, hooks)?;
                installed
                    .arguments
                    .extend([OsString::from("--plugin-dir"), plugin.into_os_string()]);
            }
            if let Some((engine, mailbox)) = &self.relay {
                let relay = crate::coord::relay::install(home, mailbox, engine)
                    .map_err(|problem| LaunchError(problem.0, None))?;
                installed.relay = true;
                installed
                    .arguments
                    .extend([OsString::from("--plugin-dir"), relay.into_os_string()]);
            }
            assert!(
                text.len()
                    <= MASTER_INSTRUCTIONS_BYTES_MAX
                        + 1
                        + LIVE_INSTRUCTIONS_BYTES_MAX
                        + TOOL_USE_BYTES_MAX
                        + SKILLS_INDEX_BYTES_MAX
                        + SECTION_SLACK
                        + crate::agent_home::PROCESS_BYTES_MAX
            );
            let instructions = state.join("instructions.md");
            write_private(&instructions, text.as_bytes())?;
            if let Some((tmux, worktree)) = self.native_team.as_ref().filter(|_| tools) {
                let mut mate = text.clone();
                mate.push_str(&teammate_section(worktree)?);
                let file = state.join(TEAMMATE_INSTRUCTIONS);
                write_private(&file, mate.as_bytes())?;
                installed.native = Some(NativeTeam {
                    tmux: tmux.clone(),
                    instructions: file,
                });
            }
            installed.arguments.extend([
                OsString::from("--append-system-prompt-file"),
                instructions.into_os_string(),
            ]);
        }
        assert!(installed.files.as_ref().is_some_and(|files| {
            files.home == home && files.removals().iter().all(|path| path.starts_with(home))
        }));
        assert!(installed.arguments.len() <= 8);
        assert!(!installed.hooks || (id == "claude-code" && !hooks.is_empty()));
        assert!(!installed.relay || id == "claude-code");
        assert!(installed.denied.len() <= DENIED_TOOLS_MAX);
        assert!(installed.denied.is_empty() || (id == "claude-code" && tools));
        assert!(installed.native.is_none() || (id == "claude-code" && tools));
        assert!(installed.model_proxy.is_none() || tools);
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

    /// Tells whether a headless Claude run of this launch keeps its transcript, as the
    /// project's "Keep transcripts for headless Claude runs" setting says.
    pub fn keeps_transcript(&self) -> bool {
        self.transcripts
    }

    /// Tells whether this launch's plugins hold hooks, the ones the owner turned on or the
    /// message relay's, so Claude Code must run with hooks allowed.
    pub fn runs_hooks(&self) -> bool {
        self.hooks || self.relay
    }

    /// Names the MCP tools the owner switched off for this Claude Code launch, as Claude Code
    /// names them (`mcp__<declared server>__<tool>`), for its deny list. Codex gets them in
    /// `config.toml` instead, so its list is empty.
    pub fn denied_tools(&self) -> &[String] {
        &self.denied
    }

    /// Tells whether this Claude Code launch is a lead that runs its own agent team.
    pub fn native_team(&self) -> bool {
        self.native.is_some()
    }

    /// Names what this native-team lead's teammates get, none for any other launch.
    pub fn native(&self) -> Option<&NativeTeam> {
        self.native.as_ref()
    }

    /// Names the loopback port of the model proxy this launch's variables point at, none
    /// for a launch that reaches its model directly.
    pub fn model_proxy(&self) -> Option<u16> {
        self.model_proxy
    }

    /// Names the display this launch's Claude Code terminal draws with (N-D9).
    pub fn claude_display(&self) -> ClaudeDisplay {
        self.claude_display
    }

    /// Names where this launch wrote its files, for the record of a pane that may outlive
    /// the engine. Output: the files, none once `keep` left them to the pane.
    pub fn files(&self) -> Option<&InstalledFiles> {
        self.files.as_ref()
    }

    /// Takes over the files an earlier engine's launch left for a pane that still runs, so
    /// they are deleted when the adopted pane stops. Input: the files from the pane record.
    /// Output: the guard, a `launch_account_home_*` code for a home that is no longer a
    /// private agent home, or `launch_home_busy` while another launch uses it.
    pub fn adopt(files: &InstalledFiles) -> Result<Self, LaunchError> {
        crate::harnesses::launch::validate_home(&files.home)?;
        let lock = lock(&files.home)?;
        Ok(Self {
            arguments: Vec::new(),
            files: Some(files.clone()),
            transcripts: false,
            hooks: false,
            relay: false,
            denied: Vec::new(),
            native: None,
            model_proxy: None,
            claude_display: ClaudeDisplay::Classic,
            _lock: lock,
        })
    }

    /// Releases the home lock but leaves the files to a pane kept running as the engine
    /// quits; a later engine adopts them from the pane record. Takes the guard.
    pub fn keep(mut self) {
        self.files = None;
    }
}

impl InstalledFiles {
    /// Lists the generated file and folder paths a launch deletes, all inside its home.
    fn removals(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.home.join(STATE)];
        if self.codex {
            paths.push(self.home.join("config.toml"));
        }
        paths
    }
}

impl Drop for Installed {
    /// Deletes the launch's files, then releases the home lock on its open file
    /// description, so a process that shares it, such as a child another thread is
    /// starting, can never keep the home busy after the launch ends.
    fn drop(&mut self) {
        if let Some(files) = &self.files {
            for path in files.removals() {
                remove(&path);
            }
            if files.codex {
                remove_copies(&files.home);
            }
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

/// Tells an agent how its work lands in its git layout: a linked worktree leaves edits for
/// CrazyAgent to land, an own clone commits on its branch. Input: the layout. Output: the
/// section, at most `LANDING_RULES_BYTES_MAX` bytes.
pub fn landing_rules(layout: Layout) -> &'static str {
    match layout {
        Layout::Linked => LANDING_RULES,
        Layout::Clone => OWN_BRANCH_LANDING_RULES,
    }
}

fn render(
    project: &ProjectId,
    setup: Option<&StoredSetup>,
    memories: &str,
    layout: Layout,
) -> String {
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
                        ContextKind::Plan => "agent-written plan",
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
    let mut text = header + landing_rules(layout) + &brief + &index;
    if !memories.is_empty() {
        text.push('\n');
        text.push_str(memories);
    }
    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX);
    text
}

/// How the answer to an `ask` call reaches the conversation that asked: at once on a live
/// master's own server, when the reply ends on the drop-file server, or through the master
/// for a sub-agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskReach {
    Live,
    ReplyEnd,
    SubAgent,
}

/// Tells a conversation that holds the engine's `ask` tool how it asks the owner: with that
/// tool, or also with a for me `update_work` question when it has the team tools section,
/// one question per call, with its options and the one it recommends, never in reply text
/// or with its harness's own question tools, and how the answer comes back. Input: how the
/// answer reaches it and whether its for me `update_work` questions reach the owner too.
/// Output: at most `ASK_RULE_BYTES_MAX` bytes of markdown.
pub(crate) fn ask_rule(reach: AskReach, for_me: bool) -> String {
    assert!(!for_me || reach != AskReach::ReplyEnd);
    let back = match reach {
        AskReach::Live => format!(
            "The owner's answer or dismissal comes to you once as a message from the owner. If the owner answers you here in chat instead, record it with `{}`, naming the question, its version and the owner's turn.",
            crate::team::tool::RECORD_TOOL
        ),
        AskReach::ReplyEnd => format!(
            "The engine logs your questions when your reply ends, and the owner's answer or dismissal comes back to you on a later turn; after a turn that failed it can come again, so act on it once. Each turn lists your questions still open. If the owner answers one in chat instead, record it with `{}`, naming the question, version 0 and the owner's message number that list gives.",
            crate::team::tool::RECORD_TOOL
        ),
        AskReach::SubAgent => "Your master sees your questions too, and CrazyAgent types the owner's answer or dismissal into your terminal once.".to_owned(),
    };
    let how = if for_me {
        format!(
            "Ask the owner a question with the `{}` tool of the `{}` MCP server, or as a for me question in `{}`; both reach the owner. Never ask in your reply text or with your harness's own question tools.",
            crate::team::tool::ASK_TOOL,
            crate::team::tool::SERVER_NAME,
            crate::team::tool::UPDATE_WORK_TOOL
        )
    } else {
        format!(
            "Ask the owner a question only with the `{}` tool of the `{}` MCP server, never in your reply text and never with your harness's own question tools.",
            crate::team::tool::ASK_TOOL,
            crate::team::tool::SERVER_NAME
        )
    };
    let rule = format!(
        "\n## Asking the owner\n\n{how} Ask one question per call and wait for its answer before the next one. Put up to {} choices in `options`, the one you recommend in `recommended`, and what the owner needs to know in `context`. Answer what you can from the code and the project's documents first. {back}\n",
        crate::questions::OPTIONS_MAX
    );
    assert!(rule.len() <= ASK_RULE_BYTES_MAX);
    rule
}

fn skill_description(path: &Path) -> String {
    crate::plugins::skill_folder::stored_description(path).unwrap_or_default()
}

/// Cuts a skill description to at most `bytes` bytes for one index line, at the last whole
/// word with "..." when it is longer. Inputs: the description and the bound, above 3.
/// Output: the description or its cut.
fn short_description(text: &str, bytes: usize) -> String {
    assert!(bytes > "...".len());
    if text.len() <= bytes {
        return text.to_owned();
    }
    let head = clip(text, bytes - "...".len());
    let whole = text[head.len()..].starts_with(char::is_whitespace);
    let cut = match head.rfind(char::is_whitespace) {
        Some(end) if !whole => &head[..end],
        _ => head,
    };
    let short = format!("{}...", cut.trim_end());
    assert!(short.len() <= bytes);
    short
}

/// Tells a tools-on launch when to use the tools it has (18.15.1): the context-mode tools
/// left on, by the names its harness gives them, graphify on the project graph and the
/// approved skills by the names the skills index uses. Inputs: whether the harness is Claude
/// Code, the launch's servers, the tools switched off and the first approved skill's name,
/// none without skills. Output: at most `TOOL_USE_BYTES_MAX` bytes of markdown.
fn tool_use_section(
    claude: bool,
    servers: &[LaunchServer],
    off_tools: &[(String, Vec<String>)],
    skill: Option<&str>,
) -> String {
    let mut section = String::from(
        "\n## Tools to use\n\nThe owner's instructions and the sections above come first. This section only names the tools this launch has as you call them; where other instructions name one another way, use the name here.\n\n",
    );
    let names = context_mode_names(claude, off_tools);
    let context_mode = servers
        .iter()
        .any(|server| server.name == CONTEXT_MODE_SERVER);
    if context_mode && !names.is_empty() {
        let listed: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
        section.push_str(&format!(
            "- context-mode: run every command, test, search, file read or web fetch whose output may pass 20 lines through {}",
            listed.join(", ")
        ));
        section.push_str(&match claude {
            true => format!(
                ", and keep Bash for short fixed output, edits and git. They load on demand: first run ToolSearch with `select:{}`.\n",
                names.join(",")
            ),
            false => format!(
                " of the `{CONTEXT_MODE_DECLARED}` MCP server, and keep the shell for short fixed output, edits and git.\n"
            ),
        });
    }
    section.push_str("- graphify: when `graphify-out/graph.json` is in your worktree, run `graphify query \"<question>\"` in the shell before you read or edit code, `graphify path \"<A>\" \"<B>\"` for how two parts connect and `graphify explain \"<name>\"` for one part.\n");
    if let Some(name) = skill {
        section.push_str(&match claude {
            true => format!(
                "- Skills: before each task, call the Skill tool for every skill in the Skills section below that fits, by its name there, such as `crazyagent:{}`. A skill other instructions name as `superpowers:NAME`, `software-practices:NAME` or plain `NAME` is `crazyagent:NAME` here.\n",
                clip(name, NAME_BYTES_MAX)
            ),
            false => "- Skills: before each task, read the SKILL.md of every skill in the Skills section below that fits and follow it. A skill other instructions name as `superpowers:NAME`, `software-practices:NAME` or `crazyagent:NAME` is the one listed as NAME there.\n".to_owned(),
        });
    }
    assert!(section.len() <= TOOL_USE_BYTES_MAX);
    section
}

/// Names the context-mode tools a launch keeps on, as its harness calls them: Claude Code by
/// their declared MCP names, Codex by the bare tool names. Inputs: whether the harness is
/// Claude Code and the tools switched off. Output: the names, in `CONTEXT_MODE_TOOLS` order.
fn context_mode_names(claude: bool, off_tools: &[(String, Vec<String>)]) -> Vec<String> {
    let off = off_tools
        .iter()
        .find(|(name, _)| name == CONTEXT_MODE_SERVER)
        .map_or(&[][..], |(_, off)| off.as_slice());
    let names: Vec<String> = CONTEXT_MODE_TOOLS
        .into_iter()
        .filter(|tool| !off.iter().any(|name| name == tool))
        .map(|tool| match claude {
            true => format!("mcp__{CONTEXT_MODE_DECLARED}__{tool}"),
            false => tool.to_owned(),
        })
        .collect();
    assert!(!names.is_empty() || off.len() >= CONTEXT_MODE_TOOLS.len());
    names
}

/// Lists the skills the owner approved for one launch, each with its one-line description
/// and the path of its `SKILL.md` copy, and says to check the list first and how to use a
/// skill: through Claude Code's Skill tool, or for Codex by reading the file. Inputs: the
/// folder the skills were copied into, the skills and whether the harness has a Skill tool.
/// Output: at most `SKILLS_INDEX_BYTES_MAX` plus slack bytes of markdown.
fn skills_index(folder: &Path, skills: &[EffectiveSkill], skill_tool: bool) -> String {
    let using = match skill_tool {
        true => SKILL_TOOL_USE,
        false => SKILL_FILE_USE,
    };
    let mut index = format!(
        "\n## Skills\n\nThe owner approved these skills for this project. Check this list first: before work that involves files, a specialised tool or a repeatable workflow, look here for a skill that fits. {using}\n\n"
    );
    let mut left = 0;
    for skill in skills {
        let name = clip(&skill.name, NAME_BYTES_MAX);
        let path = folder.join(name).join("SKILL.md");
        let description = short_description(
            skill_description(&skill.folder.join("SKILL.md")).trim(),
            SKILL_LINE_DESCRIPTION_BYTES_MAX,
        );
        let line = match description.is_empty() {
            true => format!("- {name} ({})\n", path.display()),
            false => format!("- {name}: {description} ({})\n", path.display()),
        };
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

/// Lists for a master whose own tools are off the approved skills and MCP servers that team
/// agents with tools on receive. While the owner chose skills for leads or workers (DEC-111
/// R3), each skill names the roles that get it, and a skill neither gets is left out.
/// Inputs: the project's approved plugins and its skills per role. Output: the section, or
/// nothing when no agent gets anything.
fn team_index(plugins: &EffectivePlugins, roles: &RoleSkills) -> String {
    let per_role = roles.lead.is_some() || roles.worker.is_some();
    let gets = |role, name: &str| {
        roles
            .of(role)
            .is_none_or(|chosen| chosen.iter().any(|kept| kept == name))
    };
    let skills = plugins.skills.iter().filter_map(|skill| {
        let tag = match (
            gets(SkillRole::Lead, &skill.name),
            gets(SkillRole::Worker, &skill.name),
        ) {
            (false, false) => return None,
            _ if !per_role => "",
            (true, true) => " (leads and workers)",
            (true, false) => " (leads)",
            (false, true) => " (workers)",
        };
        let description = skill_description(&skill.folder.join("SKILL.md"));
        Some(format!(
            "- Skill {}{tag}: {}\n",
            clip(&skill.name, NAME_BYTES_MAX),
            clip(description.trim(), DESCRIPTION_BYTES_MAX)
        ))
    });
    let servers = plugins
        .servers
        .iter()
        .map(|server| format!("- MCP server {}\n", clip(&server.name, NAME_BYTES_MAX)));
    let mut index = String::from(match per_role {
        false => {
            "\n## Team skills and MCP servers\n\nYour own tools are off in this conversation, so you cannot call these. The owner approved them for this project, and every team agent with tools on receives them. Plan the team's work with them in mind.\n\n"
        }
        true => {
            "\n## Team skills and MCP servers\n\nYour own tools are off in this conversation, so you cannot call these. The owner approved them for this project. Every team agent with tools on receives the MCP servers, but each skill only the team roles named after it, as the owner chose: a lead is a sub-agent that may start sub-agents of its own, and a worker is any other sub-agent or teammate. Plan the team's work with them in mind.\n\n"
        }
    });
    let head = index.len();
    let mut left = 0;
    for line in skills.chain(servers) {
        if left > 0 || index.len() + line.len() > TEAM_INDEX_BYTES_MAX {
            left += 1;
            continue;
        }
        index.push_str(&line);
    }
    if index.len() == head && left == 0 {
        return String::new();
    }
    if left > 0 {
        index.push_str(&format!("- and {left} more\n"));
    }
    assert!(index.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK);
    index
}

/// Names an approved MCP server the way an agent's harness declares it. The context-mode
/// server takes the name its own Claude Code plugin gives it, so the tool names its hooks
/// and the owner's instructions use, such as `mcp__plugin_context-mode_context-mode__ctx_search`,
/// exist for every agent and teammate. Input: the stored server name. Output: the declared name.
pub(crate) fn declared_server_name(name: &str) -> &str {
    match name {
        CONTEXT_MODE_SERVER => CONTEXT_MODE_DECLARED,
        other => other,
    }
}

fn secret_text(secret: &LaunchSecret) -> Result<&str, LaunchError> {
    std::str::from_utf8(secret.value.expose())
        .ok()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .ok_or_else(|| failure("launch_context_secret_invalid"))
}

/// Names the switched-off tools of the servers one Claude Code launch declares, the way
/// Claude Code names them: `mcp__<declared server>__<tool>`. Input: the declared servers and
/// the switched-off tools by stored server name. Output: the names, in server order.
fn denied_tools(servers: &[LaunchServer], off_tools: &[(String, Vec<String>)]) -> Vec<String> {
    let denied: Vec<String> = servers
        .iter()
        .filter_map(|server| {
            off_tools
                .iter()
                .find(|(name, _)| *name == server.name)
                .map(|(_, off)| (declared_server_name(&server.name), off))
        })
        .flat_map(|(declared, off)| {
            off.iter()
                .map(move |tool| format!("mcp__{declared}__{tool}"))
        })
        .collect();
    assert!(denied.iter().all(|name| !name.starts_with('-')));
    denied
}

fn codex_config(
    instructions: &str,
    servers: &[LaunchServer],
    off_tools: &[(String, Vec<String>)],
) -> Result<String, LaunchError> {
    let mut document = toml_edit::DocumentMut::new();
    document["developer_instructions"] = toml_edit::value(instructions);
    document["project_doc_max_bytes"] = toml_edit::value(0);
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
            if let Some((_, off)) = off_tools.iter().find(|(name, _)| *name == server.name) {
                entry["disabled_tools"] =
                    toml_edit::value(off.iter().map(String::as_str).collect::<toml_edit::Array>());
            }
            let name = declared_server_name(&server.name);
            if table.contains_key(name) {
                return Err(failure("launch_context_server_name_taken"));
            }
            table.insert(name, toml_edit::Item::Table(entry));
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
        let name = declared_server_name(&server.name);
        if declared.contains_key(name) {
            return Err(failure("launch_context_server_name_taken"));
        }
        declared.insert(name.to_owned(), entry);
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

/// Takes a context-less launch's hold on an agent home: the same per-home lock a full
/// install takes, installing no files. The guard keeps a second launch out of the home
/// while this one runs, so writing the launch key never lands on a home an agent already
/// runs on. Input: the agent's home. Output: the lock guard, or `launch_home_busy`.
pub fn lock_only(home: &Path) -> Result<Installed, LaunchError> {
    crate::harnesses::launch::validate_home(home)?;
    let lock = lock(home)?;
    Ok(Installed {
        arguments: Vec::new(),
        files: None,
        transcripts: false,
        hooks: false,
        relay: false,
        denied: Vec::new(),
        native: None,
        model_proxy: None,
        claude_display: ClaudeDisplay::Classic,
        _lock: lock,
    })
}

/// Removes the Codex skill copies at `<home>/skills/crazyagent` and Codex's own system
/// skills at `<home>/skills/.system`, which the agent can write during a run and Codex would
/// keep while their marker matches, so Codex installs them fresh at its next start (LL2S-1).
/// It never resolves `skills` by path, because the agent may have replaced that folder with
/// a link: each entry is moved up into the verified home through descriptors and removed
/// there. A `skills` that is not a real folder of this user is left alone. Input: the
/// validated agent home.
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
    for (entry, kind) in [(STATE, "skills"), (CODEX_SYSTEM_SKILLS, "system")] {
        let parked = format!(".{STATE}-{kind}-{}", std::process::id());
        remove(&home.join(&parked));
        if rustix::fs::renameat(&skills, entry, &parent, parked.as_str()).is_ok() {
            remove(&home.join(&parked));
        }
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

/// Writes the hooks one Claude launch runs into its generated plugin: each hook's stored
/// plugin folder copied once under `hook-plugins/<folder id>`, and `hooks/hooks.json`
/// holding every hook with its command run from that copy, so `CLAUDE_PLUGIN_ROOT` names
/// the hook's own plugin files while the skills stay in the generated plugin. A plugin
/// path a shell double quote cannot hold gets no hooks. Inputs: the generated plugin
/// folder and the hooks. Output: whether hooks were written, or a launch refusal.
fn write_hooks(plugin: &Path, hooks: &[EffectiveHook]) -> Result<bool, LaunchError> {
    let quotable = plugin.to_str().is_some_and(|text| {
        !text
            .chars()
            .any(|character| character.is_control() || "\"$`\\".contains(character))
    });
    if hooks.is_empty() || !quotable {
        return Ok(false);
    }
    let copies = plugin.join(HOOK_PLUGINS);
    directory(&copies, true)?;
    let mut tally = hook_folder::Tally::default();
    let mut copied: Vec<&Path> = Vec::new();
    let mut events = serde_json::Map::new();
    for hook in hooks {
        let id = hook
            .folder
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| crate::protocol::single_component(name))
            .ok_or_else(|| failure("launch_context_hook_unavailable"))?;
        if !copied.contains(&hook.folder.as_path()) {
            hook_folder::copy_stored(&hook.folder, &copies.join(id), &mut tally).map_err(
                |code| match code {
                    "hooks_too_large" => failure("launch_context_too_large"),
                    _ => failure("launch_context_hook_unavailable"),
                },
            )?;
            copied.push(&hook.folder);
        }
        let command = format!(
            "{HOOK_ROOT}=\"{PLUGIN_ROOT}/{HOOK_PLUGINS}/{id}\"; export CLAUDE_PLUGIN_ROOT=\"${HOOK_ROOT}\"; {}",
            hook.command
                .replace(PLUGIN_ROOT, &format!("${{{HOOK_ROOT}}}"))
        );
        let mut entry = serde_json::json!({"type": "command", "command": command});
        if let Some(timeout) = hook.timeout {
            entry["timeout"] = timeout.into();
        }
        let mut group = serde_json::json!({"hooks": [entry]});
        if let Some(matcher) = &hook.matcher {
            group["matcher"] = matcher.clone().into();
        }
        match events
            .entry(hook.event.clone())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        {
            serde_json::Value::Array(groups) => groups.push(group),
            _ => unreachable!("each event holds a list of groups"),
        }
    }
    assert!(!copied.is_empty() && copied.len() <= hooks.len());
    directory(&plugin.join("hooks"), true)?;
    write_private(
        &plugin.join("hooks").join("hooks.json"),
        serde_json::json!({"hooks": events}).to_string().as_bytes(),
    )?;
    Ok(true)
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
