use crate::harnesses::launch::LaunchError;
use crate::memory::{KnowledgeEntry, KnowledgeStatus};
use crate::plugins::{LaunchPlugins, LaunchSecret, LaunchServer, ServerSpec};
use crate::project_setup::{ContextKind, StoredSetup};
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction, params};
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const HEADER_BYTES_MAX: usize = 1536;
pub const BRIEF_BYTES_MAX: usize = crate::project_setup::CONTEXT_CHUNK_BYTES;
pub const INDEX_ITEMS_MAX: usize = crate::project_setup::CONTEXT_ITEMS_MAX;
pub const INDEX_BYTES_MAX: usize = 7 * 1024;
pub const MEMORY_ITEMS_MAX: usize = 24;
pub const MEMORY_BYTES_MAX: usize = 12 * 1024;
pub const INSTRUCTIONS_BYTES_MAX: usize = 32 * 1024;
pub const SKILL_FILES_MAX: usize = 4096;
pub const SKILL_BYTES_MAX: u64 = 64 * 1024 * 1024;
const SKILL_DEPTH_MAX: usize = 8;
const SECTION_SLACK: usize = 512;
const MEMORY_ROWS_MAX: i64 = 512;
const NAME_BYTES_MAX: usize = 256;
const FOLDER_BYTES_MAX: usize = 1024;
const STATE: &str = "crazyagent";
const LOCK: &str = "crazyagent.lock";
const PLUGIN_MANIFEST: &str = "{\"name\":\"crazyagent\",\"version\":\"1.0.0\",\"description\":\"Skills the owner approved for this project in CrazyAgent\"}\n";

const _: () = assert!(
    HEADER_BYTES_MAX
        + BRIEF_BYTES_MAX
        + INDEX_BYTES_MAX
        + MEMORY_BYTES_MAX
        + 4 * SECTION_SLACK
        + MEMORY_ITEMS_MAX * 32
        <= INSTRUCTIONS_BYTES_MAX
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
}

/// The files one launch wrote into its agent home, and the lock that keeps a second launch
/// out of that home. Dropping it deletes every file holding a secret and the copied skills.
#[derive(Debug)]
pub struct Installed {
    arguments: Vec<OsString>,
    removals: Vec<PathBuf>,
    _lock: File,
}

struct Memory {
    global: bool,
    revision: u64,
    text: String,
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
    let root = connection
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .and_then(Path::parent)
        .filter(|root| root.is_absolute())
        .ok_or_else(|| failure("launch_context_database_unknown"))?
        .to_path_buf();
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    let plugins = crate::storage::plugins::launch_set(&tx, &root, project)
        .map_err(|problem| LaunchError(problem.0))?;
    let setup = crate::project_setup::load_setup(&tx, project).map_err(unavailable)?;
    let memories = approved_memories(&tx, project)?;
    drop(tx);
    let instructions = render(project, setup.as_ref(), &memories);
    assert!(instructions.len() <= INSTRUCTIONS_BYTES_MAX);
    Ok(AgentContext {
        plugins,
        instructions,
    })
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
            _lock: lock,
        };
        remove(&state);
        directory(&state, true)?;
        let instructions = state.join("instructions.md");
        write_private(&instructions, self.instructions.as_bytes())?;
        let servers: &[LaunchServer] = if tools { &self.plugins.servers } else { &[] };
        let skills = tools && !self.plugins.skills.is_empty();
        if id == "codex" {
            let config = home.join("config.toml");
            let copies = home.join("skills").join(STATE);
            installed.removals.extend([config.clone(), copies.clone()]);
            remove(&copies);
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
            if skills {
                let plugin = state.join("plugin");
                directory(&plugin, true)?;
                directory(&plugin.join(".claude-plugin"), true)?;
                write_private(
                    &plugin.join(".claude-plugin").join("plugin.json"),
                    PLUGIN_MANIFEST.as_bytes(),
                )?;
                self.copy_skills(&plugin.join("skills"))?;
                installed
                    .arguments
                    .extend([OsString::from("--plugin-dir"), plugin.into_os_string()]);
            }
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
    fn drop(&mut self) {
        for path in &self.removals {
            remove(path);
        }
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

fn render(project: &ProjectId, setup: Option<&StoredSetup>, memories: &[Memory]) -> String {
    let mut header = String::from("# Project context from CrazyAgent\n\n");
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
            if setup.folder.len() <= FOLDER_BYTES_MAX {
                header.push_str(&format!("Project folder: {}\n", setup.folder));
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
                index.push_str("\n## Context files\n\nThese files are inside the project folder. Open them when they help with your task.\n\n");
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
    let memory = render_memories(memories);
    assert!(header.len() <= HEADER_BYTES_MAX);
    assert!(brief.len() <= BRIEF_BYTES_MAX + SECTION_SLACK);
    assert!(index.len() <= INDEX_BYTES_MAX + SECTION_SLACK);
    let text = header + &brief + &index + &memory;
    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX);
    text
}

fn render_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut section = String::from(
        "\n## Approved memories\n\nThe owner approved these notes. Use them as background knowledge for this project.\n",
    );
    let mut used = 0;
    let mut given = 0;
    for memory in memories {
        let text = memory.text.trim();
        if given == MEMORY_ITEMS_MAX || used + text.len() > MEMORY_BYTES_MAX {
            break;
        }
        section.push_str(if memory.global {
            "\n### From your global memory\n\n"
        } else {
            "\n### From this project\n\n"
        });
        section.push_str(text);
        section.push('\n');
        used += text.len();
        given += 1;
    }
    if given < memories.len() {
        section.push_str("\nSome approved memories were left out to keep this short.\n");
    }
    assert!(given <= MEMORY_ITEMS_MAX && used <= MEMORY_BYTES_MAX);
    assert!(section.len() <= MEMORY_BYTES_MAX + SECTION_SLACK + MEMORY_ITEMS_MAX * 32);
    section
}

fn approved_memories(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Vec<Memory>, LaunchError> {
    let mut statement = tx
        .prepare(
            "SELECT e.body FROM knowledge_heads h JOIN knowledge_entries e ON h.id=e.id AND h.version=e.version
             WHERE h.project=?1 OR EXISTS(SELECT 1 FROM knowledge_globals g
                 WHERE g.entry=h.id AND g.version=h.version AND g.approved=1)
             ORDER BY h.rowid DESC LIMIT ?2",
        )
        .map_err(unavailable)?;
    let bodies = statement
        .query_map(params![project.as_str(), MEMORY_ROWS_MAX], |row| {
            row.get::<_, String>(0)
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let mut memories = Vec::new();
    for body in bodies {
        let entry: KnowledgeEntry = serde_json::from_str(&body).map_err(unavailable)?;
        let local = &entry.project_id == project;
        let approved = if local {
            entry.status == KnowledgeStatus::Confirmed && entry.human_reviewed
        } else {
            matches!(
                entry.status,
                KnowledgeStatus::Provisional | KnowledgeStatus::Confirmed
            ) && globally_approved(tx, &entry)?
        };
        if approved && sources_live(tx, &entry)? {
            memories.push(Memory {
                global: !local,
                revision: entry.revision.value(),
                text: entry.text,
            });
        }
    }
    memories.sort_by(|a, b| a.global.cmp(&b.global).then(b.revision.cmp(&a.revision)));
    assert!(memories.len() as i64 <= MEMORY_ROWS_MAX);
    Ok(memories)
}

fn globally_approved(tx: &Transaction<'_>, entry: &KnowledgeEntry) -> Result<bool, LaunchError> {
    let version =
        i64::try_from(entry.version).map_err(|_| failure("launch_context_unavailable"))?;
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM knowledge_globals WHERE entry=?1 AND version=?2 AND hash=?3 AND approved=1)",
        params![entry.id.as_str(), version, entry.content_hash],
        |row| row.get(0),
    )
    .map_err(|_| failure("launch_context_unavailable"))
}

fn sources_live(tx: &Transaction<'_>, entry: &KnowledgeEntry) -> Result<bool, LaunchError> {
    for source in &entry.source_ids {
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM knowledge_sources WHERE project=?1 AND id=?2 AND revoked=0)",
                params![entry.project_id.as_str(), source.as_str()],
                |row| row.get(0),
            )
            .map_err(|_| failure("launch_context_unavailable"))?;
        if !live {
            return Ok(false);
        }
    }
    Ok(true)
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

#[cfg(test)]
#[path = "launch_context/tests.rs"]
mod tests;
