use super::*;
use crate::coord::tool_switches::{self, BUILTIN_TOOLS, CRAZYAGENT_SERVER, Switch};
use crate::plugins::{
    EffectiveHook, EffectivePlugins, EffectiveSecret, EffectiveServer, EffectiveSkill,
    HOOKS_PER_SCOPE_MAX, HookView, LaunchPlugins, LaunchSecret, LaunchServer,
    SECRETS_PER_SERVER_MAX, SERVERS_PER_SCOPE_MAX, SKILLS_PER_SCOPE_MAX, Scope, SecretValue,
    SecretView, ServerSpec, ServerStatus, ServerView, SkillView, TOOL_NAME_BYTES_MAX,
    TOOL_NAMES_BYTES_MAX, TOOLS_MAX, ToolView, hook_folder, skill_folder,
};
use crate::storage::versioned;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

#[cfg(any(unix, test))]
macro_rules! items_columns {
    () => {
        "(
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    kind TEXT NOT NULL CHECK(kind IN ('skill','server','hook')),
    scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 160),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536),
    UNIQUE(kind, scope, name)
)"
    };
}

#[cfg(any(unix, test))]
const SCHEMA: &str = concat!(
    "CREATE TABLE IF NOT EXISTS plugin_items ",
    items_columns!(),
    ";
CREATE TABLE IF NOT EXISTS plugin_switches (
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    item TEXT NOT NULL CHECK(length(item) BETWEEN 1 AND 64),
    PRIMARY KEY(project, item)
);"
);
/// Rebuilds an item table from before hooks with the current check, keeping every row, for
/// the upgrade step that lets it hold hooks.
#[cfg(unix)]
pub(super) const WIDEN_ITEMS: &str = concat!(
    "CREATE TABLE plugin_items_next ",
    items_columns!(),
    ";
INSERT INTO plugin_items_next(id,kind,scope,name,body) SELECT id,kind,scope,name,body FROM plugin_items;
DROP TABLE plugin_items;
ALTER TABLE plugin_items_next RENAME TO plugin_items;"
);
pub(crate) const HOOKS: &str = "hooks";
const PAGE_BYTES_MAX: usize = 48 * 1024;
pub(crate) const STORE: &str = "plugins";
pub(crate) const SECRETS: &str = "secrets";

pub(crate) enum Command {
    AddSkill {
        scope: Scope,
        source: String,
    },
    ImportSkill {
        source: String,
        replace: Option<String>,
    },
    SetSkill {
        id: String,
        enabled: bool,
        version: u64,
    },
    RemoveSkill {
        id: String,
        version: u64,
    },
    AddHooks {
        scope: Scope,
        source: String,
        follows: Option<String>,
    },
    SetHook {
        id: String,
        enabled: bool,
    },
    Hooks {
        project: Option<ProjectId>,
        offset: u16,
    },
    AddServer {
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<EffectiveSecret>,
    },
    UpdateServer {
        id: String,
        spec: ServerSpec,
        version: u64,
    },
    Approve {
        id: String,
        spec: ServerSpec,
    },
    SetServer {
        id: String,
        enabled: bool,
        version: u64,
    },
    RemoveServer {
        id: String,
        version: u64,
    },
    Switch {
        project: ProjectId,
        id: String,
        on: bool,
        version: u64,
    },
    AttachSecret {
        id: String,
        name: String,
        reference: String,
    },
    DetachSecret {
        id: String,
        name: String,
    },
    GetServer {
        id: String,
    },
    Server {
        id: String,
    },
    RecordCheck {
        id: String,
        digest: String,
        outcome: Result<Vec<String>, String>,
    },
    Skills {
        project: Option<ProjectId>,
        offset: u16,
    },
    Skill {
        id: String,
        project: Option<ProjectId>,
    },
    Servers {
        project: Option<ProjectId>,
        offset: u16,
    },
    Effective {
        project: ProjectId,
    },
    Launch {
        project: ProjectId,
    },
    Tools {
        project: Option<ProjectId>,
        server: Option<String>,
    },
    SetTool {
        project: Option<ProjectId>,
        server: Option<String>,
        tool: String,
        switch: Switch,
    },
    RestoreTools {
        project: Option<ProjectId>,
        server: Option<String>,
    },
}

pub(crate) struct SkillChange {
    pub(crate) view: SkillView,
    pub(crate) added: bool,
}

pub(crate) enum ResultValue {
    Skill(Box<SkillChange>),
    Imported(Box<SkillChange>, Vec<String>, usize),
    SkillRemoved(PathBuf),
    Server(Box<ServerView>),
    Removed(Vec<String>),
    Skills(Vec<SkillView>, Option<u16>),
    Servers(Vec<ServerView>, Option<u16>),
    Secret(Box<ServerView>, Option<String>),
    Check(
        Box<ServerSpec>,
        Vec<EffectiveSecret>,
        crate::sandbox::SandboxNetwork,
    ),
    Effective(Box<EffectivePlugins>),
    Launch(Box<LaunchPlugins>),
    HooksAdded(Vec<HookView>, PathBuf),
    Hooks(Vec<HookView>, Option<u16>),
    Hook(Box<HookView>),
    Tools(Vec<ToolView>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillRecord {
    id: String,
    name: String,
    description: String,
    scope: Scope,
    enabled: bool,
    source: String,
    folder: String,
}

/// One stored hook: its plugin, event, matcher, command and timeout as the plugin defined
/// them, the store-relative folder copy its command runs from, where it applies, whether it
/// is on, the skill or MCP server id it follows and the folder it was added from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookRecord {
    id: String,
    name: String,
    plugin: String,
    event: String,
    matcher: Option<String>,
    command: String,
    timeout: Option<u32>,
    scope: Scope,
    enabled: bool,
    follows: Option<String>,
    source: String,
    folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckRecord {
    digest: String,
    tools: Vec<String>,
    problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerRecord {
    id: String,
    name: String,
    scope: Scope,
    spec: ServerSpec,
    enabled: bool,
    approved: Option<String>,
    secrets: Vec<EffectiveSecret>,
    check: Option<CheckRecord>,
    checks: u64,
}

/// Creates the skill, MCP server and hook tables when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(error)
}

fn scope_key(scope: &Scope) -> String {
    match scope {
        Scope::Global => "global".into(),
        Scope::Project { project_id } => format!("project:{}", project_id.as_str()),
    }
}

fn new_id(prefix: &str) -> Result<String, EngineError> {
    let random = crate::engine::owner::random_id()?;
    Ok(format!("{prefix}-{}", &random[..24]))
}

/// Refuses the private plugin store where Unix owner and mode checks do not exist.
/// Takes the folder path, unused.
/// Returns `plugin_store_unavailable` until Windows has its own owner-only folder check.
#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), EngineError> {
    Err(error("plugin_store_unavailable"))
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), EngineError> {
    let unavailable = || error("plugin_store_unavailable");
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(unavailable()),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o7777 != 0o700
    {
        return Err(error("plugin_store_unsafe"));
    }
    Ok(())
}

/// Opens the private plugin store beside the engine database, creating missing folders.
/// Input: the engine root and the folders below `plugins`. Output: the folder path.
pub(crate) fn store_folder(root: &Path, parts: &[&str]) -> Result<PathBuf, EngineError> {
    let mut path = root.join(STORE);
    private_directory(&path)?;
    for part in parts {
        assert!(crate::protocol::single_component(part));
        path.push(part);
        private_directory(&path)?;
    }
    Ok(path)
}

#[cfg(unix)]
const SECRET_BYTES_MAX: usize = 8192;
/// The shortest MCP secret value kept. A shorter value such as `1` or `true` is part of
/// nearly every launch path or flag, so no launch could prove its command keeps it hidden.
#[cfg(unix)]
const SECRET_BYTES_MIN: usize = 8;

fn valid_reference(reference: &str) -> bool {
    reference.strip_prefix("secret-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

#[cfg(unix)]
fn secrets_directory(root: &Path) -> Result<std::fs::File, EngineError> {
    let path = store_folder(root, &[SECRETS])?;
    let directory = std::fs::File::from(
        rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| error("plugin_secret_store_unavailable"))?,
    );
    let metadata = directory
        .metadata()
        .map_err(|_| error("plugin_secret_store_unavailable"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(error("plugin_store_unsafe"));
    }
    Ok(directory)
}

/// Makes a fresh reference for one MCP server secret. Output: `secret-` and 32 hex digits.
pub(crate) fn new_secret_reference() -> Result<String, EngineError> {
    let random = crate::engine::owner::random_id()?;
    let reference = format!("secret-{}", &random[..32]);
    assert!(valid_reference(&reference));
    Ok(reference)
}

/// Saves one MCP server secret as a private 0600 file named by its reference.
/// Inputs: the engine root, a fresh reference and the secret bytes, which are zeroed after
/// writing. Output: Ok, or a code with no file left behind.
#[cfg(not(unix))]
pub(crate) fn store_secret(
    _root: &Path,
    _reference: &str,
    mut secret: Vec<u8>,
) -> Result<(), EngineError> {
    secret.fill(0);
    Err(error("plugin_secret_store_unavailable"))
}

/// Saves one MCP server secret as a private 0600 file named by its reference.
/// Inputs: the engine root, a fresh reference and the secret bytes, which are zeroed after
/// writing. Output: Ok, or a code with no file left behind; `server_secret_too_short` for a
/// value under `SECRET_BYTES_MIN` bytes.
#[cfg(unix)]
pub(crate) fn store_secret(
    root: &Path,
    reference: &str,
    mut secret: Vec<u8>,
) -> Result<(), EngineError> {
    let stored = (|| {
        if !valid_reference(reference) || secret.is_empty() || secret.len() > SECRET_BYTES_MAX {
            return Err(error("server_secret_invalid"));
        }
        if secret.len() < SECRET_BYTES_MIN {
            return Err(error("server_secret_too_short"));
        }
        let directory = secrets_directory(root)?;
        let mut file = std::fs::File::from(
            rustix::fs::openat(
                &directory,
                reference,
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode(0o600),
            )
            .map_err(|_| error("plugin_secret_store_unavailable"))?,
        );
        if std::io::Write::write_all(&mut file, &secret)
            .and_then(|()| file.sync_all())
            .is_err()
        {
            let _ = rustix::fs::unlinkat(&directory, reference, rustix::fs::AtFlags::empty());
            return Err(error("plugin_secret_store_unavailable"));
        }
        directory
            .sync_all()
            .map_err(|_| error("plugin_secret_store_unavailable"))
    })();
    secret.fill(0);
    stored
}

/// Reads one MCP server secret for a launch or a check. Inputs: the engine root and the
/// reference from the effective set. Output: the secret bytes; callers zero them after use.
#[cfg(not(unix))]
pub(crate) fn read_secret(_root: &Path, _reference: &str) -> Result<Vec<u8>, EngineError> {
    Err(error("plugin_secret_store_unavailable"))
}

/// Reads one MCP server secret for a launch or a check. Inputs: the engine root and the
/// reference from the effective set. Output: the secret bytes; callers zero them after use.
#[cfg(unix)]
pub(crate) fn read_secret(root: &Path, reference: &str) -> Result<Vec<u8>, EngineError> {
    use std::io::Read;
    if !valid_reference(reference) {
        return Err(error("server_secret_not_found"));
    }
    let directory = secrets_directory(root)?;
    let file = std::fs::File::from(
        rustix::fs::openat(
            &directory,
            reference,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| error("server_secret_not_found"))?,
    );
    let metadata = file
        .metadata()
        .map_err(|_| error("plugin_secret_store_unavailable"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() as usize > SECRET_BYTES_MAX
    {
        return Err(error("plugin_store_unsafe"));
    }
    let mut bytes = Vec::with_capacity(SECRET_BYTES_MAX);
    if file
        .take(SECRET_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > SECRET_BYTES_MAX
    {
        bytes.fill(0);
        return Err(error("plugin_secret_store_unavailable"));
    }
    Ok(bytes)
}

/// Deletes one MCP server secret file. Inputs: the engine root and the reference.
/// Output: Ok when the file is gone, including when it was already missing.
#[cfg(not(unix))]
pub(crate) fn remove_secret(_root: &Path, _reference: &str) -> Result<(), EngineError> {
    Err(error("plugin_secret_store_unavailable"))
}

/// Deletes one MCP server secret file. Inputs: the engine root and the reference.
/// Output: Ok when the file is gone, including when it was already missing.
#[cfg(unix)]
pub(crate) fn remove_secret(root: &Path, reference: &str) -> Result<(), EngineError> {
    if !valid_reference(reference) {
        return Ok(());
    }
    let directory = secrets_directory(root)?;
    match rustix::fs::unlinkat(&directory, reference, rustix::fs::AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err(error("plugin_secret_store_unavailable")),
    }
}

/// The folder under the engine root where an MCP server check keeps what it wrote, one
/// folder per server id.
pub(crate) const CHECK_FOLDER: &str = "mcp-checks";

/// The name prefix of a removed MCP server's check folder moved aside in the check folder
/// to be deleted: `.removed-<server id>-<32 hex digits>`.
const REMOVED_CHECK: &str = ".removed-";

/// Moves one removed MCP server's check folder aside to `.removed-<id>-<nonce>` in the same
/// folder with one rename, so its path is free at once however much a check wrote there.
/// Inputs: the engine's check folder and the server id. Output: the moved folder, none when
/// there was no folder, `plugin_check_folder_invalid` for a name that is not one plain
/// folder name, or `plugin_check_folder_not_removed`.
pub(crate) fn move_check_folder_aside(
    checks: &Path,
    id: &str,
) -> Result<Option<PathBuf>, EngineError> {
    check_folder_name(id)?;
    let nonce = crate::engine::owner::random_id()?;
    let name = format!("{REMOVED_CHECK}{id}-{}", &nonce[..32]);
    assert!(moved_aside(&name, id));
    let aside = checks.join(name);
    match std::fs::rename(checks.join(id), &aside) {
        Ok(()) => Ok(Some(aside)),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error("plugin_check_folder_not_removed")),
    }
}

/// Deletes one removed MCP server's check folder with everything in it, and each copy of it
/// moved aside for deletion that a crash left, for the start sweep. Inputs: the engine root
/// and the server id. Output: Ok when all are gone, including when already missing;
/// `plugin_check_folder_invalid` for a name that is not one plain folder name.
#[cfg(unix)]
pub(crate) fn remove_check_folder(root: &Path, id: &str) -> Result<(), EngineError> {
    check_folder_name(id)?;
    let checks = root.join(CHECK_FOLDER);
    let mut folders = vec![checks.join(id)];
    match std::fs::read_dir(&checks) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|_| error("plugin_check_folder_not_removed"))?;
                if moved_aside(&entry.file_name().to_string_lossy(), id) {
                    folders.push(entry.path());
                }
            }
        }
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(error("plugin_check_folder_not_removed")),
    }
    for folder in folders {
        match std::fs::remove_dir_all(folder) {
            Ok(()) => {}
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error("plugin_check_folder_not_removed")),
        }
    }
    Ok(())
}

fn check_folder_name(id: &str) -> Result<(), EngineError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(error("plugin_check_folder_invalid"));
    }
    Ok(())
}

fn moved_aside(name: &str, id: &str) -> bool {
    name.strip_prefix(REMOVED_CHECK)
        .and_then(|rest| rest.rsplit_once('-'))
        .is_some_and(|(moved, nonce)| {
            moved == id && nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// Deletes one removed project's skill store folder with everything in it, for the start
/// sweep. Inputs: the engine root and the project id. Output: Ok when the folder is gone,
/// including when it was already missing; `plugin_skill_folder_invalid` for a name that is
/// not one store-relative folder name, on Windows also for a colon, which Windows reads as a
/// stream or device suffix instead of that folder; elsewhere a project id's colon names the
/// folder as it always has.
pub(crate) fn remove_skill_project_folder(root: &Path, project: &str) -> Result<(), EngineError> {
    if !crate::protocol::single_component(project) || (cfg!(windows) && project.contains(':')) {
        return Err(error("plugin_skill_folder_invalid"));
    }
    match std::fs::remove_dir_all(
        root.join(STORE)
            .join("skills")
            .join("project")
            .join(project),
    ) {
        Ok(()) => Ok(()),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(error("plugin_skill_folder_not_removed")),
    }
}

/// Deletes one stored hook folder the trash names, all its hooks being gone. Inputs: the
/// engine root and the folder's id. Output: Ok when it is gone, or a plain refusal.
#[cfg(unix)]
pub(crate) fn remove_hook_folder(root: &Path, id: &str) -> Result<(), EngineError> {
    if !valid_folder_id(id) {
        return Err(error("plugin_hook_folder_invalid"));
    }
    match std::fs::remove_dir_all(root.join(STORE).join(HOOKS).join(id)) {
        Ok(()) => Ok(()),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(error("plugin_hook_folder_not_removed")),
    }
}

fn valid_folder_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Names the store folder id of a hook's folder copy, the last part of its store path.
fn folder_id(record: &HookRecord) -> Option<&str> {
    Path::new(&record.folder)
        .file_name()?
        .to_str()
        .filter(|id| valid_folder_id(id))
}

/// Tells whether any stored hook follows one skill or MCP server. Inputs: the transaction
/// and the item id. Output: true when one does.
#[cfg(unix)]
pub(super) fn hooks_follow(tx: &Transaction<'_>, item: &str) -> Result<bool, EngineError> {
    Ok(all::<HookRecord>(tx, "hook")?
        .iter()
        .any(|hook| hook.follows.as_deref() == Some(item)))
}

/// Deletes every hook that follows one skill or MCP server, with the item, and notes each
/// hook folder copy for the trash to delete once nothing refers to it. Inputs: the
/// transaction and the item id. Output: any SQLite failure.
fn remove_following(tx: &Transaction<'_>, item: &str) -> Result<(), EngineError> {
    let hooks: Vec<HookRecord> = all::<HookRecord>(tx, "hook")?
        .into_iter()
        .filter(|hook| hook.follows.as_deref() == Some(item))
        .collect();
    for hook in &hooks {
        tx.execute("DELETE FROM plugin_items WHERE id=?1", [&hook.id])
            .map_err(error)?;
        tx.execute("DELETE FROM plugin_switches WHERE item=?1", [&hook.id])
            .map_err(error)?;
        if let Some(id) = folder_id(hook) {
            super::trash::put(tx, super::trash::HOOK_FOLDER, id)?;
        }
    }
    Ok(())
}

/// Deletes every listed MCP server secret file, going on past one that fails so an
/// earlier failure leaves none of the others behind. Inputs: the engine root and the
/// references. Output: Ok when every file is gone, else the first failure.
pub(crate) fn remove_secrets(root: &Path, references: &[String]) -> Result<(), EngineError> {
    references
        .iter()
        .map(|reference| remove_secret(root, reference))
        .fold(Ok(()), Result::and)
}

/// Deletes every plugin item and switch scoped to one project, and trashes each project MCP
/// server's secret files and check folder and, when the project has skills, its skill store
/// folder, all swept after commit. A new project that reuses the removed project's id inherits
/// none of its MCP servers, secrets, skills or per-project switches, and a rolled back removal
/// keeps every file. Safe to call for an id with no scoped rows, where it changes nothing.
/// Inputs: the transaction and the project. Output: any SQLite failure.
pub(crate) fn remove_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    let scope = Scope::Project {
        project_id: project.clone(),
    };
    let key = scope_key(&scope);
    let servers: Vec<ServerRecord> = all::<ServerRecord>(tx, "server")?
        .into_iter()
        .filter(|server| server.scope == scope)
        .collect();
    for server in &servers {
        for secret in &server.secrets {
            super::trash::put(tx, super::trash::MCP_SECRET, &secret.reference)?;
        }
        super::trash::put(tx, super::trash::MCP_CHECKS, &server.id)?;
    }
    let skills: i64 = tx
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE kind='skill' AND scope=?1",
            [&key],
            |row| row.get(0),
        )
        .map_err(error)?;
    for hook in all::<HookRecord>(tx, "hook")?
        .iter()
        .filter(|hook| hook.scope == scope)
    {
        if let Some(id) = folder_id(hook) {
            super::trash::put(tx, super::trash::HOOK_FOLDER, id)?;
        }
    }
    tx.execute(
        "DELETE FROM plugin_switches WHERE project=?1",
        [project.as_str()],
    )
    .map_err(error)?;
    let removed_items = tx
        .execute("DELETE FROM plugin_items WHERE scope=?1", [&key])
        .map_err(error)?;
    assert!(removed_items >= servers.len());
    if skills > 0 {
        super::trash::put(tx, super::trash::SKILL_FOLDER, project.as_str())?;
    }
    Ok(())
}

fn project_exists(tx: &Transaction<'_>, project: &ProjectId) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project.as_str()],
        |row| row.get(0),
    )
    .map_err(error)
}

fn require_scope(tx: &Transaction<'_>, scope: &Scope) -> Result<(), EngineError> {
    if let Scope::Project { project_id } = scope
        && !project_exists(tx, project_id)?
    {
        return Err(error("plugin_project_unknown"));
    }
    Ok(())
}

fn count(tx: &Transaction<'_>, kind: &str, scope: &str) -> Result<usize, EngineError> {
    let total: i64 = tx
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE kind=?1 AND scope=?2",
            [kind, scope],
            |row| row.get(0),
        )
        .map_err(error)?;
    Ok(total as usize)
}

fn name_taken(
    tx: &Transaction<'_>,
    kind: &str,
    scope: &str,
    name: &str,
) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE kind=?1 AND scope=?2 AND name=?3)",
        [kind, scope, name],
        |row| row.get(0),
    )
    .map_err(error)
}

fn load<T: for<'de> Deserialize<'de>>(
    tx: &Transaction<'_>,
    kind: &str,
    id: &str,
) -> Result<T, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM plugin_items WHERE id=?1 AND kind=?2",
            [id, kind],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let body = body.ok_or_else(|| error(format!("{kind}_not_found")))?;
    serde_json::from_str(&body).map_err(|_| error("plugin_record_invalid"))
}

fn all<T: for<'de> Deserialize<'de>>(
    tx: &Transaction<'_>,
    kind: &str,
) -> Result<Vec<T>, EngineError> {
    let mut statement = tx
        .prepare("SELECT body FROM plugin_items WHERE kind=?1 ORDER BY scope, name")
        .map_err(error)?;
    let bodies = statement
        .query_map([kind], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    bodies
        .iter()
        .map(|body| serde_json::from_str(body).map_err(|_| error("plugin_record_invalid")))
        .collect()
}

fn save<T: Serialize>(
    tx: &Transaction<'_>,
    kind: &str,
    id: &str,
    scope: &Scope,
    name: &str,
    record: &T,
) -> Result<(), EngineError> {
    let body = serde_json::to_string(record).map_err(error)?;
    if body.len() > 65536 {
        return Err(error("plugin_record_bounds"));
    }
    tx.execute(
        "INSERT INTO plugin_items(id,kind,scope,name,body) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        params![id, kind, scope_key(scope), name, body],
    )
    .map_err(error)?;
    Ok(())
}

fn switched_off(tx: &Transaction<'_>, project: &ProjectId) -> Result<Vec<String>, EngineError> {
    let mut statement = tx
        .prepare("SELECT item FROM plugin_switches WHERE project=?1")
        .map_err(error)?;
    let items = statement
        .query_map([project.as_str()], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(items)
}

/// The network a global server's plugin check runs under: the strictest choice among the
/// projects that enable the server, so the check reaches no further than any of their agents
/// may; Open when no project enables it. A project enables a global server unless it is in
/// that project's switched-off list. Takes the transaction and the server id. Returns the
/// sandbox network, or `project_network_unreadable` when a project's policy cannot be read.
fn global_check_network(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<crate::sandbox::SandboxNetwork, EngineError> {
    use crate::sandbox::policy::AgentNetwork;
    let mut ids: Vec<String> = Vec::new();
    {
        let mut statement = tx.prepare("SELECT id FROM projects").map_err(error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?;
        for row in rows {
            ids.push(row.map_err(error)?);
        }
    }
    let rank = |network: AgentNetwork| {
        AgentNetwork::ALL
            .iter()
            .position(|choice| *choice == network)
            .unwrap_or(0)
    };
    let mut strictest: Option<AgentNetwork> = None;
    for project in ids {
        let project = ProjectId::new(&project).map_err(|_| error("project_network_unreadable"))?;
        if switched_off(tx, &project)?.iter().any(|item| item == id) {
            continue;
        }
        let network = crate::sandbox::policy::load(tx, project.as_str())
            .map_err(|_| error("project_network_unreadable"))?
            .network;
        let replace = strictest.is_none_or(|current| rank(network) > rank(current));
        if replace {
            strictest = Some(network);
        }
    }
    Ok(strictest
        .map(|network| network.sandbox(None))
        .unwrap_or(crate::sandbox::SandboxNetwork::Open))
}

fn visible(scope: &Scope, project: Option<&ProjectId>) -> bool {
    match (scope, project) {
        (Scope::Global, _) => true,
        (Scope::Project { project_id }, Some(project)) => project_id == project,
        (Scope::Project { .. }, None) => false,
    }
}

fn on_here(scope: &Scope, id: &str, project: Option<&ProjectId>, off: &[String]) -> Option<bool> {
    match (scope, project) {
        (Scope::Global, Some(_)) => Some(!off.iter().any(|item| item == id)),
        _ => None,
    }
}

/// Reads one plugin's version, 0 for a plugin the owner never changed.
pub(super) fn plugin_version(connection: &Connection, id: &str) -> Result<u64, EngineError> {
    Ok(versioned::current(connection, &versioned::plugin_item(id)?)?.version)
}

/// Writes the next version of one plugin when the owner's screen showed the current one,
/// before its On switch, project switches or command change. Takes the transaction, the
/// plugin id and the version shown. Returns the plain refusal `stale_revision` otherwise.
fn owner_change(tx: &Transaction<'_>, id: &str, version: u64) -> Result<(), EngineError> {
    match versioned::owner_write(tx, &versioned::plugin_item(id)?, version)? {
        Ok(next) => {
            assert_eq!(next, version + 1);
            Ok(())
        }
        Err(_) => Err(error(versioned::STALE_CODE)),
    }
}

fn skill_view(
    connection: &Connection,
    record: &SkillRecord,
    root: &Path,
    on_here: Option<bool>,
) -> Result<SkillView, EngineError> {
    let description = crate::memory::visible_text(&record.description);
    let folder = root.join(&record.folder);
    let refused = crate::plugins::skill_folder::stored_description(&folder.join("SKILL.md"))
        .err()
        .map(str::to_owned);
    Ok(SkillView {
        id: record.id.clone(),
        name: record.name.clone(),
        line: crate::skills::one_line(&description),
        description,
        scope: record.scope.clone(),
        scope_label: record.scope.label().into(),
        enabled: record.enabled,
        on_here,
        source: crate::memory::visible_text(&record.source),
        origin: crate::skills::origin::of(root, &record.source),
        folder: folder.display().to_string(),
        version: plugin_version(connection, &record.id)?,
        refused,
    })
}

fn hook_view(
    connection: &Connection,
    record: &HookRecord,
    follows: Option<String>,
    on_here: Option<bool>,
) -> Result<HookView, EngineError> {
    Ok(HookView {
        id: record.id.clone(),
        name: record.name.clone(),
        plugin: record.plugin.clone(),
        event: record.event.clone(),
        matcher: record.matcher.clone(),
        command: crate::memory::visible_text(&record.command),
        scope: record.scope.clone(),
        scope_label: record.scope.label().into(),
        enabled: record.enabled,
        on_here,
        follows,
        source: crate::memory::visible_text(&record.source),
        version: plugin_version(connection, &record.id)?,
    })
}

/// Names the skill or MCP server a hook follows, as the hook list shows it.
fn follows_name(tx: &Transaction<'_>, record: &HookRecord) -> Result<Option<String>, EngineError> {
    let Some(item) = &record.follows else {
        return Ok(None);
    };
    tx.query_row("SELECT name FROM plugin_items WHERE id=?1", [item], |row| {
        row.get(0)
    })
    .optional()
    .map_err(error)
}

/// Reads a plugin folder's command hooks and stores each one, turned on, with one bounded
/// copy of the files they run from in the plugin store; a hook with `follows` runs only
/// while that skill or MCP server is on. Hooks are named `<plugin>/<event>/<n>`. Inputs:
/// the transaction, engine root, scope, the absolute plugin folder and the item to follow.
/// Output: the stored hooks and their folder copy, or a plain refusal with nothing stored.
fn add_hooks(
    tx: &Transaction<'_>,
    root: &Path,
    scope: Scope,
    source: String,
    follows: Option<String>,
) -> Result<(Vec<HookRecord>, PathBuf), EngineError> {
    require_scope(tx, &scope)?;
    if let Some(item) = &follows {
        let kind: Option<String> = tx
            .query_row("SELECT kind FROM plugin_items WHERE id=?1", [item], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        if !matches!(kind.as_deref(), Some("skill" | "server")) {
            return Err(error("plugin_not_found"));
        }
    }
    let folder = std::fs::canonicalize(&source).map_err(|_| error("skill_folder_not_found"))?;
    let found = hook_folder::read(&folder).map_err(error)?;
    let key = scope_key(&scope);
    if count(tx, "hook", &key)? + found.hooks.len() > HOOKS_PER_SCOPE_MAX {
        return Err(error("hook_limit"));
    }
    let names: Vec<String> = (0..found.hooks.len())
        .map(|index| {
            let hook = &found.hooks[index];
            let same = found.hooks[..index]
                .iter()
                .filter(|other| other.event == hook.event)
                .count();
            format!("{}/{}/{}", found.plugin, hook.event, same + 1)
        })
        .collect();
    for name in &names {
        assert!(name.len() <= 64);
        if name_taken(tx, "hook", &key, name)? {
            return Err(error("hooks_already_added"));
        }
    }
    let id = new_id("hooks")?;
    let parent = store_folder(root, &[HOOKS])?;
    let copy = parent.join(&id);
    hook_folder::copy(&folder, &copy, &found.hooks).map_err(error)?;
    let relative = copy
        .strip_prefix(root)
        .map_err(|_| error("plugin_store_unavailable"))?
        .display()
        .to_string();
    let mut records = Vec::with_capacity(found.hooks.len());
    let stored = (|| {
        for (hook, name) in found.hooks.iter().zip(names) {
            let record = HookRecord {
                id: new_id("hook")?,
                name,
                plugin: found.plugin.clone(),
                event: hook.event.clone(),
                matcher: hook.matcher.clone(),
                command: hook.command.clone(),
                timeout: hook.timeout,
                scope: scope.clone(),
                enabled: true,
                follows: follows.clone(),
                source: source.clone(),
                folder: relative.clone(),
            };
            save(tx, "hook", &record.id, &record.scope, &record.name, &record)?;
            records.push(record);
        }
        Ok(())
    })();
    if let Err(problem) = stored {
        remove_folder(&copy);
        return Err(problem);
    }
    assert_eq!(records.len(), found.hooks.len());
    Ok((records, copy))
}

/// Lists the hooks agents launched for a project run: each hook that is on, from global
/// settings unless the project switched it off or from the project itself, whose followed
/// skill or MCP server is on there too, with its stored folder copy. Inputs: the open
/// transaction, the engine root and the project. Output: the hooks, ordered by event,
/// plugin and name.
pub(crate) fn effective_hooks(
    tx: &Transaction<'_>,
    root: &Path,
    project: &ProjectId,
) -> Result<Vec<EffectiveHook>, EngineError> {
    let off = switched_off(tx, project)?;
    let usable = |scope: &Scope, id: &str| match scope {
        Scope::Global => !off.iter().any(|item| item == id),
        Scope::Project { project_id } => project_id == project,
    };
    let mut on: Vec<String> = all::<SkillRecord>(tx, "skill")?
        .into_iter()
        .filter(|skill| skill.enabled && usable(&skill.scope, &skill.id))
        .map(|skill| skill.id)
        .collect();
    on.extend(
        all::<ServerRecord>(tx, "server")?
            .into_iter()
            .filter(|server| {
                status(server) == ServerStatus::Approved && usable(&server.scope, &server.id)
            })
            .map(|server| server.id),
    );
    let mut hooks: Vec<(String, EffectiveHook)> = all::<HookRecord>(tx, "hook")?
        .into_iter()
        .filter(|hook| hook.enabled && usable(&hook.scope, &hook.id))
        .filter(|hook| hook.follows.as_ref().is_none_or(|item| on.contains(item)))
        .filter(|hook| folder_id(hook).is_some())
        .map(|hook| {
            (
                hook.name,
                EffectiveHook {
                    plugin: hook.plugin,
                    event: hook.event,
                    matcher: hook.matcher,
                    command: hook.command,
                    timeout: hook.timeout,
                    folder: root.join(hook.folder),
                },
            )
        })
        .collect();
    hooks.sort_by(|a, b| (&a.1.event, &a.1.plugin, &a.0).cmp(&(&b.1.event, &b.1.plugin, &b.0)));
    assert!(hooks.len() <= 2 * HOOKS_PER_SCOPE_MAX);
    Ok(hooks.into_iter().map(|(_, hook)| hook).collect())
}

fn status(record: &ServerRecord) -> ServerStatus {
    let digest = record.spec.digest();
    let failed = record
        .check
        .as_ref()
        .is_some_and(|check| check.digest == digest && check.problem.is_some());
    if !record.enabled {
        ServerStatus::Off
    } else if record.approved.as_deref() != Some(digest.as_str()) {
        ServerStatus::WaitingForApproval
    } else if failed {
        ServerStatus::CantStart
    } else {
        ServerStatus::Approved
    }
}

fn secret_saved(root: &Path, reference: &str) -> bool {
    std::fs::symlink_metadata(root.join(STORE).join(SECRETS).join(reference))
        .is_ok_and(|metadata| metadata.is_file())
}

fn server_view(
    connection: &Connection,
    record: &ServerRecord,
    root: &Path,
    on_here: Option<bool>,
) -> Result<ServerView, EngineError> {
    let status = status(record);
    let digest = record.spec.digest();
    let current = record.check.as_ref().filter(|check| check.digest == digest);
    Ok(ServerView {
        id: record.id.clone(),
        name: record.name.clone(),
        scope: record.scope.clone(),
        scope_label: record.scope.label().into(),
        spec: record.spec.clone(),
        runs: record.spec.runs(),
        enabled: record.enabled,
        status,
        status_label: status.label().into(),
        problem: current
            .and_then(|check| check.problem.as_deref())
            .map(crate::plugins::check_problem),
        tools: current.map(|check| check.tools.clone()).unwrap_or_default(),
        checks: record.checks,
        secrets: record
            .secrets
            .iter()
            .map(|secret| SecretView {
                name: secret.name.clone(),
                saved: secret_saved(root, &secret.reference),
            })
            .collect(),
        on_here,
        version: plugin_version(connection, &record.id)?,
    })
}

fn page<T: Serialize>(items: Vec<T>, offset: u16) -> Result<(Vec<T>, Option<u16>), EngineError> {
    let start = usize::from(offset);
    let mut kept = Vec::new();
    let mut used = 0usize;
    let total = items.len();
    for item in items.into_iter().skip(start) {
        let size = serde_json::to_vec(&item).map_err(error)?.len() + 1;
        if !kept.is_empty() && used + size > PAGE_BYTES_MAX {
            break;
        }
        used += size;
        kept.push(item);
    }
    let end = start + kept.len();
    let next = if end < total {
        Some(u16::try_from(end).map_err(|_| error("plugin_page_bounds"))?)
    } else {
        None
    };
    assert!(next.is_none() || !kept.is_empty());
    Ok((kept, next))
}

fn remove_folder(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn add_skill(
    tx: &Transaction<'_>,
    root: &Path,
    scope: Scope,
    source: String,
    rules: &skill_folder::Rules,
) -> Result<(SkillRecord, skill_folder::SkillMeta), EngineError> {
    require_scope(tx, &scope)?;
    let key = scope_key(&scope);
    if count(tx, "skill", &key)? >= SKILLS_PER_SCOPE_MAX {
        return Err(error("skill_limit"));
    }
    let id = new_id("skill")?;
    let parts: Vec<String> = match &scope {
        Scope::Global => vec!["skills".into(), "global".into()],
        Scope::Project { project_id } => {
            vec![
                "skills".into(),
                "project".into(),
                project_id.as_str().into(),
            ]
        }
    };
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let parent = store_folder(root, &parts)?;
    let staging = parent.join(format!(".{id}"));
    let meta = skill_folder::copy_with(Path::new(&source), &staging, rules).map_err(error)?;
    let result = (|| {
        if name_taken(tx, "skill", &key, &meta.name)? {
            return Err(error("skill_name_taken"));
        }
        let folder = parent.join(&meta.name);
        if std::fs::symlink_metadata(&folder).is_ok() {
            return Err(error("plugin_store_unavailable"));
        }
        std::fs::rename(&staging, &folder).map_err(|_| error("plugin_store_unavailable"))?;
        let relative = folder
            .strip_prefix(root)
            .map_err(|_| error("plugin_store_unavailable"))?
            .display()
            .to_string();
        let record = SkillRecord {
            id: id.clone(),
            name: meta.name.clone(),
            description: meta.description.clone(),
            scope: scope.clone(),
            enabled: true,
            source: source.clone(),
            folder: relative,
        };
        if let Err(problem) = save(tx, "skill", &id, &scope, &meta.name, &record) {
            remove_folder(&folder);
            return Err(problem);
        }
        Ok(record)
    })();
    if result.is_err() {
        remove_folder(&staging);
    }
    result.map(|record| (record, meta))
}

fn replace_skill(
    tx: &Transaction<'_>,
    root: &Path,
    id: &str,
    source: String,
) -> Result<(SkillRecord, skill_folder::SkillMeta), EngineError> {
    let mut record: SkillRecord = load(tx, "skill", id)?;
    if record.scope != Scope::Global {
        return Err(error("skill_not_found"));
    }
    let parent = store_folder(root, &["skills", "global"])?;
    let folder = root.join(&record.folder);
    if folder.parent() != Some(parent.as_path()) {
        return Err(error("plugin_store_unsafe"));
    }
    let staging = parent.join(format!(".{}", new_id("skill")?));
    let meta = skill_folder::copy_with(Path::new(&source), &staging, &skill_folder::IMPORTED)
        .map_err(error)?;
    let old = parent.join(format!(".{}-old", record.id));
    let result = (|| {
        if meta.name != record.name {
            return Err(error("skill_import_name_changed"));
        }
        if std::fs::symlink_metadata(&old).is_ok() {
            return Err(error("plugin_store_unavailable"));
        }
        std::fs::rename(&folder, &old).map_err(|_| error("plugin_store_unavailable"))?;
        if std::fs::rename(&staging, &folder).is_err() {
            let _ = std::fs::rename(&old, &folder);
            return Err(error("plugin_store_unavailable"));
        }
        record.description = meta.description.clone();
        record.source = source.clone();
        record.enabled = true;
        if let Err(problem) = save(
            tx,
            "skill",
            &record.id,
            &record.scope,
            &record.name,
            &record,
        ) {
            remove_folder(&folder);
            let _ = std::fs::rename(&old, &folder);
            return Err(problem);
        }
        remove_folder(&old);
        Ok(())
    })();
    if result.is_err() {
        remove_folder(&staging);
    }
    result.map(|()| (record, meta))
}

fn add_server(
    tx: &Transaction<'_>,
    scope: Scope,
    name: String,
    spec: ServerSpec,
    secrets: Vec<EffectiveSecret>,
) -> Result<ServerRecord, EngineError> {
    crate::plugins::validate_server_name(&name)?;
    spec.validate()?;
    let names: Vec<&str> = secrets.iter().map(|secret| secret.name.as_str()).collect();
    crate::plugins::validate_secret_names(&names)?;
    require_scope(tx, &scope)?;
    let key = scope_key(&scope);
    if count(tx, "server", &key)? >= SERVERS_PER_SCOPE_MAX {
        return Err(error("server_limit"));
    }
    if name_taken(tx, "server", &key, &name)? {
        return Err(error("server_name_taken"));
    }
    let record = ServerRecord {
        id: new_id("mcp")?,
        name,
        scope,
        spec,
        enabled: true,
        approved: None,
        secrets,
        check: None,
        checks: 0,
    };
    save(
        tx,
        "server",
        &record.id,
        &record.scope,
        &record.name,
        &record,
    )?;
    Ok(record)
}

fn put_server(tx: &Transaction<'_>, record: &ServerRecord) -> Result<(), EngineError> {
    save(
        tx,
        "server",
        &record.id,
        &record.scope,
        &record.name,
        record,
    )
}

fn valid_tools(tools: Vec<String>) -> Result<Vec<String>, String> {
    if tools.len() > TOOLS_MAX
        || tools.iter().map(String::len).sum::<usize>() > TOOL_NAMES_BYTES_MAX
        || tools.iter().any(|tool| {
            tool.is_empty()
                || tool.len() > TOOL_NAME_BYTES_MAX
                || tool.chars().any(char::is_control)
        })
    {
        return Err("server_reply_invalid".into());
    }
    Ok(tools)
}

/// Lists exactly what agents launched for a project receive: approved global items that the
/// project did not switch off, plus the project's own enabled items, where a project item
/// replaces a global one with the same name. Inputs: the open transaction, the engine root
/// and the project. Output: skill folder paths and MCP servers with secret references.
pub(crate) fn effective(
    tx: &Transaction<'_>,
    root: &Path,
    project: &ProjectId,
) -> Result<EffectivePlugins, EngineError> {
    let off = switched_off(tx, project)?;
    let usable = |scope: &Scope, id: &str| match scope {
        Scope::Global => !off.iter().any(|item| item == id),
        Scope::Project { project_id } => project_id == project,
    };
    let skills: Vec<SkillRecord> = all::<SkillRecord>(tx, "skill")?
        .into_iter()
        .filter(|skill| skill.enabled && usable(&skill.scope, &skill.id))
        .collect();
    let servers: Vec<ServerRecord> = all::<ServerRecord>(tx, "server")?
        .into_iter()
        .filter(|server| {
            status(server) == ServerStatus::Approved && usable(&server.scope, &server.id)
        })
        .collect();
    let local = |scope: &Scope| matches!(scope, Scope::Project { .. });
    let mut plugins = EffectivePlugins::default();
    for skill in &skills {
        let shadowed = !local(&skill.scope)
            && skills
                .iter()
                .any(|other| local(&other.scope) && other.name == skill.name);
        let folder = root.join(&skill.folder);
        let described =
            crate::plugins::skill_folder::stored_description(&folder.join("SKILL.md")).is_ok();
        if !shadowed && described {
            plugins.skills.push(EffectiveSkill {
                name: skill.name.clone(),
                folder,
            });
        }
    }
    for server in &servers {
        let shadowed = !local(&server.scope)
            && servers
                .iter()
                .any(|other| local(&other.scope) && other.name == server.name);
        if !shadowed {
            plugins.servers.push(EffectiveServer {
                name: server.name.clone(),
                spec: server.spec.clone(),
                secrets: server.secrets.clone(),
            });
        }
    }
    plugins.skills.sort_by(|a, b| a.name.cmp(&b.name));
    plugins.servers.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(
        plugins
            .skills
            .windows(2)
            .all(|pair| pair[0].name != pair[1].name)
    );
    assert!(
        plugins
            .servers
            .windows(2)
            .all(|pair| pair[0].name != pair[1].name)
    );
    Ok(plugins)
}

/// Reads the launch set of one project inside a caller's transaction, for launchers that
/// hold their own engine database connection. Inputs: the transaction, the engine root and
/// the project. Output: the effective set with secret values read, or `plugin_project_unknown`.
pub(crate) fn launch_set(
    tx: &Transaction<'_>,
    root: &Path,
    project: &ProjectId,
) -> Result<LaunchPlugins, EngineError> {
    if !project_exists(tx, project)? {
        return Err(error("plugin_project_unknown"));
    }
    launch(root, effective(tx, root, project)?)
}

fn launch(root: &Path, plugins: EffectivePlugins) -> Result<LaunchPlugins, EngineError> {
    let mut servers = Vec::with_capacity(plugins.servers.len());
    let mut context_mode_off = None;
    for server in plugins.servers {
        #[cfg(unix)]
        let spec = crate::context_mode::for_launch(root, server.spec);
        #[cfg(not(unix))]
        let spec: Result<ServerSpec, crate::plugins::ContextModeOff> = Ok(server.spec);
        let spec = match spec {
            Ok(spec) => spec,
            Err(off) => {
                context_mode_off = Some(off);
                continue;
            }
        };
        let mut secrets = Vec::with_capacity(server.secrets.len());
        for secret in server.secrets {
            secrets.push(LaunchSecret {
                name: secret.name,
                value: SecretValue::new(read_secret(root, &secret.reference)?),
            });
        }
        servers.push(LaunchServer {
            name: server.name,
            spec,
            secrets,
        });
    }
    assert!(servers.len() <= 2 * SERVERS_PER_SCOPE_MAX);
    Ok(LaunchPlugins {
        skills: plugins.skills,
        servers,
        context_mode_off,
    })
}

/// Runs one plugins command inside a single transaction.
/// Inputs: the engine connection, the engine root holding the plugin store and the command.
/// Output: the changed or listed items, or a plain rejection code with nothing changed.
pub(crate) fn execute(
    connection: &mut Connection,
    root: &Path,
    command: Command,
) -> Result<ResultValue, EngineError> {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    if let Command::SetSkill { id, .. }
    | Command::RemoveSkill { id, .. }
    | Command::UpdateServer { id, .. }
    | Command::SetServer { id, .. }
    | Command::RemoveServer { id, .. } = &command
    {
        super::setup::plugin_changed(&tx, id)?;
    }
    let value = run(&tx, root, command)?;
    let committed = tx.commit().map_err(error);
    match (&value, committed) {
        (ResultValue::Skill(skill) | ResultValue::Imported(skill, ..), Err(problem))
            if skill.added =>
        {
            remove_folder(Path::new(&skill.view.folder));
            Err(problem)
        }
        (ResultValue::HooksAdded(_, folder), Err(problem)) => {
            remove_folder(folder);
            Err(problem)
        }
        (_, Err(problem)) => Err(problem),
        (ResultValue::SkillRemoved(folder), Ok(())) => {
            remove_folder(folder);
            Ok(value)
        }
        (_, Ok(())) => Ok(value),
    }
}

pub(super) fn run(
    tx: &Transaction<'_>,
    root: &Path,
    command: Command,
) -> Result<ResultValue, EngineError> {
    Ok(match command {
        Command::AddSkill { scope, source } => {
            let (record, _) = add_skill(tx, root, scope, source, &skill_folder::PICKED)?;
            ResultValue::Skill(Box::new(SkillChange {
                view: skill_view(tx, &record, root, None)?,
                added: true,
            }))
        }
        Command::ImportSkill { source, replace } => {
            let added = replace.is_none();
            let (record, meta) = match replace {
                None => add_skill(tx, root, Scope::Global, source, &skill_folder::IMPORTED)?,
                Some(id) => replace_skill(tx, root, &id, source)?,
            };
            ResultValue::Imported(
                Box::new(SkillChange {
                    view: skill_view(tx, &record, root, None)?,
                    added,
                }),
                meta.skipped,
                meta.skipped_total,
            )
        }
        Command::SetSkill {
            id,
            enabled,
            version,
        } => {
            let mut record: SkillRecord = load(tx, "skill", &id)?;
            owner_change(tx, &id, version)?;
            record.enabled = enabled;
            save(
                tx,
                "skill",
                &record.id,
                &record.scope,
                &record.name,
                &record,
            )?;
            ResultValue::Skill(Box::new(SkillChange {
                view: skill_view(tx, &record, root, None)?,
                added: false,
            }))
        }
        Command::RemoveSkill { id, version } => {
            let record: SkillRecord = load(tx, "skill", &id)?;
            owner_change(tx, &id, version)?;
            versioned::forget(tx, &versioned::plugin_item(&id)?)?;
            tx.execute("DELETE FROM plugin_items WHERE id=?1", [&id])
                .map_err(error)?;
            tx.execute("DELETE FROM plugin_switches WHERE item=?1", [&id])
                .map_err(error)?;
            remove_following(tx, &id)?;
            ResultValue::SkillRemoved(root.join(&record.folder))
        }
        Command::AddHooks {
            scope,
            source,
            follows,
        } => {
            let (records, copy) = add_hooks(tx, root, scope, source, follows)?;
            let mut views = Vec::with_capacity(records.len());
            for record in &records {
                views.push(hook_view(tx, record, follows_name(tx, record)?, None)?);
            }
            ResultValue::HooksAdded(views, copy)
        }
        Command::SetHook { id, enabled } => {
            let mut record: HookRecord = load(tx, "hook", &id)?;
            record.enabled = enabled;
            save(tx, "hook", &record.id, &record.scope, &record.name, &record)?;
            let follows = follows_name(tx, &record)?;
            ResultValue::Hook(Box::new(hook_view(tx, &record, follows, None)?))
        }
        Command::Hooks { project, offset } => {
            let off = match &project {
                Some(project) => switched_off(tx, project)?,
                None => Vec::new(),
            };
            let mut views = Vec::new();
            for record in all::<HookRecord>(tx, "hook")?
                .iter()
                .filter(|record| visible(&record.scope, project.as_ref()))
            {
                let here = on_here(&record.scope, &record.id, project.as_ref(), &off);
                views.push(hook_view(tx, record, follows_name(tx, record)?, here)?);
            }
            let (views, next) = page(views, offset)?;
            ResultValue::Hooks(views, next)
        }
        Command::AddServer {
            scope,
            name,
            spec,
            secrets,
        } => {
            let record = add_server(tx, scope, name, spec, secrets)?;
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::UpdateServer { id, spec, version } => {
            spec.validate()?;
            let mut record: ServerRecord = load(tx, "server", &id)?;
            owner_change(tx, &id, version)?;
            if record.spec != spec {
                record.spec = spec;
                record.approved = None;
                record.check = None;
                put_server(tx, &record)?;
            }
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::Approve { id, spec } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            if record.spec != spec {
                return Err(error("server_changed"));
            }
            record.approved = Some(record.spec.digest());
            put_server(tx, &record)?;
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::SetServer {
            id,
            enabled,
            version,
        } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            owner_change(tx, &id, version)?;
            record.enabled = enabled;
            put_server(tx, &record)?;
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::RemoveServer { id, version } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            owner_change(tx, &id, version)?;
            versioned::forget(tx, &versioned::plugin_item(&id)?)?;
            tx.execute("DELETE FROM plugin_items WHERE id=?1", [&id])
                .map_err(error)?;
            tx.execute("DELETE FROM plugin_switches WHERE item=?1", [&id])
                .map_err(error)?;
            remove_following(tx, &id)?;
            for secret in &record.secrets {
                super::trash::put(tx, super::trash::MCP_SECRET, &secret.reference)?;
            }
            super::trash::put(tx, super::trash::MCP_CHECKS, &id)?;
            ResultValue::Removed(record.secrets.into_iter().map(|s| s.reference).collect())
        }
        Command::Switch {
            project,
            id,
            on,
            version,
        } => {
            if !project_exists(tx, &project)? {
                return Err(error("plugin_project_unknown"));
            }
            let scope: Option<String> = tx
                .query_row("SELECT scope FROM plugin_items WHERE id=?1", [&id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(error)?;
            match scope.as_deref() {
                None => return Err(error("plugin_not_found")),
                Some("global") => {}
                Some(_) => return Err(error("plugin_switch_global_only")),
            }
            owner_change(tx, &id, version)?;
            if on {
                tx.execute(
                    "DELETE FROM plugin_switches WHERE project=?1 AND item=?2",
                    [project.as_str(), &id],
                )
                .map_err(error)?;
            } else {
                tx.execute(
                    "INSERT OR IGNORE INTO plugin_switches(project,item) VALUES(?1,?2)",
                    [project.as_str(), &id],
                )
                .map_err(error)?;
            }
            ResultValue::Removed(Vec::new())
        }
        Command::AttachSecret {
            id,
            name,
            reference,
        } => {
            crate::plugins::validate_secret_name(&name)?;
            let mut record: ServerRecord = load(tx, "server", &id)?;
            let replaced = match record.secrets.iter_mut().find(|secret| secret.name == name) {
                Some(secret) => Some(std::mem::replace(&mut secret.reference, reference)),
                None => {
                    if record.secrets.len() >= SECRETS_PER_SERVER_MAX {
                        return Err(error("server_secret_limit"));
                    }
                    record.secrets.push(EffectiveSecret { name, reference });
                    None
                }
            };
            put_server(tx, &record)?;
            if let Some(old) = &replaced {
                super::trash::put(tx, super::trash::MCP_SECRET, old)?;
            }
            ResultValue::Secret(Box::new(server_view(tx, &record, root, None)?), replaced)
        }
        Command::DetachSecret { id, name } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            let index = record
                .secrets
                .iter()
                .position(|secret| secret.name == name)
                .ok_or_else(|| error("server_secret_not_found"))?;
            let removed = record.secrets.remove(index);
            put_server(tx, &record)?;
            super::trash::put(tx, super::trash::MCP_SECRET, &removed.reference)?;
            ResultValue::Secret(
                Box::new(server_view(tx, &record, root, None)?),
                Some(removed.reference),
            )
        }
        Command::Server { id } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::GetServer { id } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            let network = match &record.scope {
                Scope::Project { project_id } => {
                    crate::sandbox::policy::load(tx, project_id.as_str())
                        .map_err(|_| error("project_network_unreadable"))?
                        .network
                        .sandbox(None)
                }
                Scope::Global => global_check_network(tx, &id)?,
            };
            ResultValue::Check(Box::new(record.spec), record.secrets, network)
        }
        Command::RecordCheck {
            id,
            digest,
            outcome,
        } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            record.checks += 1;
            if record.spec.digest() == digest {
                let (tools, problem) = match outcome.and_then(valid_tools) {
                    Ok(tools) => (tools, None),
                    Err(code) => (Vec::new(), Some(code)),
                };
                record.check = Some(CheckRecord {
                    digest,
                    tools,
                    problem,
                });
            }
            put_server(tx, &record)?;
            ResultValue::Server(Box::new(server_view(tx, &record, root, None)?))
        }
        Command::Skills { project, offset } => {
            let off = match &project {
                Some(project) => switched_off(tx, project)?,
                None => Vec::new(),
            };
            let views: Vec<SkillView> = all::<SkillRecord>(tx, "skill")?
                .iter()
                .filter(|record| visible(&record.scope, project.as_ref()))
                .map(|record| {
                    skill_view(
                        tx,
                        record,
                        root,
                        on_here(&record.scope, &record.id, project.as_ref(), &off),
                    )
                })
                .collect::<Result<_, EngineError>>()?;
            let (views, next) = page(views, offset)?;
            ResultValue::Skills(views, next)
        }
        Command::Skill { id, project } => {
            let record: SkillRecord = load(tx, "skill", &id)?;
            if !visible(&record.scope, project.as_ref()) {
                return Err(error("skill_not_found"));
            }
            let off = match &project {
                Some(project) => switched_off(tx, project)?,
                None => Vec::new(),
            };
            let here = on_here(&record.scope, &record.id, project.as_ref(), &off);
            ResultValue::Skill(Box::new(SkillChange {
                view: skill_view(tx, &record, root, here)?,
                added: false,
            }))
        }
        Command::Servers { project, offset } => {
            let off = match &project {
                Some(project) => switched_off(tx, project)?,
                None => Vec::new(),
            };
            let views: Vec<ServerView> = all::<ServerRecord>(tx, "server")?
                .iter()
                .filter(|record| visible(&record.scope, project.as_ref()))
                .map(|record| {
                    server_view(
                        tx,
                        record,
                        root,
                        on_here(&record.scope, &record.id, project.as_ref(), &off),
                    )
                })
                .collect::<Result<_, EngineError>>()?;
            let (views, next) = page(views, offset)?;
            ResultValue::Servers(views, next)
        }
        Command::Effective { project } => {
            if !project_exists(tx, &project)? {
                return Err(error("plugin_project_unknown"));
            }
            ResultValue::Effective(Box::new(effective(tx, root, &project)?))
        }
        Command::Launch { project } => {
            ResultValue::Launch(Box::new(launch_set(tx, root, &project)?))
        }
        Command::Tools { project, server } => {
            ResultValue::Tools(tool_views(tx, root, project.as_ref(), server.as_deref())?.1)
        }
        Command::SetTool {
            project,
            server,
            tool,
            switch,
        } => {
            let (key, views) = tool_views(tx, root, project.as_ref(), server.as_deref())?;
            let Some(found) = views.iter().find(|view| view.tool == tool) else {
                return Err(error("tool_unknown"));
            };
            if switch == Switch::Hidden && !found.builtin {
                return Err(error("tool_delete_builtin_only"));
            }
            tool_switches::set(tx, project.as_ref(), &key, &tool, switch)?;
            let (_, views) = tool_views(tx, root, project.as_ref(), server.as_deref())?;
            assert!(views.iter().any(|view| view.tool == tool));
            ResultValue::Tools(views)
        }
        Command::RestoreTools { project, server } => {
            let (key, _) = tool_views(tx, root, project.as_ref(), server.as_deref())?;
            tool_switches::restore_defaults(tx, project.as_ref(), &key)?;
            ResultValue::Tools(tool_views(tx, root, project.as_ref(), server.as_deref())?.1)
        }
    })
}

/// Lists one server's tools with their switches: the CrazyAgent built-in tools when no server
/// id is given, else the checked tools of that MCP server, keyed by its name as agents see it.
/// A project-only server is listed only in its own project.
/// Inputs: the transaction, the plugin store root, the project (none for every project) and the
/// server id. Output: the switch key of the server and its tools in order, or a plain refusal.
fn tool_views(
    tx: &Transaction<'_>,
    root: &Path,
    project: Option<&ProjectId>,
    server: Option<&str>,
) -> Result<(String, Vec<ToolView>), EngineError> {
    if let Some(project) = project
        && !project_exists(tx, project)?
    {
        return Err(error("plugin_project_unknown"));
    }
    let (key, tools): (String, Vec<(String, String)>) = match server {
        None => (
            CRAZYAGENT_SERVER.to_owned(),
            BUILTIN_TOOLS
                .iter()
                .map(|(tool, description)| ((*tool).to_owned(), (*description).to_owned()))
                .collect(),
        ),
        Some(id) => {
            let record: ServerRecord = load(tx, "server", id)?;
            let view = server_view(tx, &record, root, None)?;
            if let Scope::Project { project_id } = &view.scope
                && project != Some(project_id)
            {
                return Err(error("server_not_found"));
            }
            let tools = view.tools.into_iter().map(|tool| (tool, String::new()));
            (view.name, tools.collect())
        }
    };
    let global: std::collections::BTreeMap<String, Switch> =
        tool_switches::overrides(tx, None, &key)?
            .into_iter()
            .collect();
    let here: std::collections::BTreeMap<String, Switch> = match project {
        Some(project) => tool_switches::overrides(tx, Some(project), &key)?
            .into_iter()
            .collect(),
        None => std::collections::BTreeMap::new(),
    };
    let views: Vec<ToolView> = tools
        .into_iter()
        .map(|(tool, description)| ToolView {
            server: key.clone(),
            server_id: server.map(str::to_owned),
            global: global.get(&tool).copied().unwrap_or(Switch::On),
            here: here.get(&tool).copied(),
            builtin: server.is_none(),
            tool,
            description,
        })
        .collect();
    assert!(views.len() <= TOOLS_MAX.max(BUILTIN_TOOLS.len()));
    assert!(project.is_some() || views.iter().all(|view| view.here.is_none()));
    Ok((key, views))
}

#[cfg(all(test, windows))]
mod windows_tests {
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
}

#[cfg(all(test, unix))]
#[path = "plugins_tests.rs"]
mod tests;
