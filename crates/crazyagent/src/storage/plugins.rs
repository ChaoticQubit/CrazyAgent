use super::*;
use crate::plugins::{
    EffectivePlugins, EffectiveSecret, EffectiveServer, EffectiveSkill, LaunchPlugins,
    LaunchSecret, LaunchServer, SECRETS_PER_SERVER_MAX, SERVERS_PER_SCOPE_MAX,
    SKILLS_PER_SCOPE_MAX, Scope, SecretValue, SecretView, ServerSpec, ServerStatus, ServerView,
    SkillView, TOOL_NAME_BYTES_MAX, TOOLS_MAX, skill_folder,
};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS plugin_items (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    kind TEXT NOT NULL CHECK(kind IN ('skill','server')),
    scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 160),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536),
    UNIQUE(kind, scope, name)
);
CREATE TABLE IF NOT EXISTS plugin_switches (
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    item TEXT NOT NULL CHECK(length(item) BETWEEN 1 AND 64),
    PRIMARY KEY(project, item)
);";
const PAGE_BYTES_MAX: usize = 48 * 1024;
const STORE: &str = "plugins";
pub(crate) const SECRETS: &str = "secrets";

pub(crate) enum Command {
    AddSkill {
        scope: Scope,
        source: String,
    },
    SetSkill {
        id: String,
        enabled: bool,
    },
    RemoveSkill {
        id: String,
    },
    AddServer {
        scope: Scope,
        name: String,
        spec: ServerSpec,
    },
    UpdateServer {
        id: String,
        spec: ServerSpec,
    },
    Approve {
        id: String,
        spec: ServerSpec,
    },
    SetServer {
        id: String,
        enabled: bool,
    },
    RemoveServer {
        id: String,
    },
    Switch {
        project: ProjectId,
        id: String,
        on: bool,
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
}

pub(crate) struct SkillChange {
    pub(crate) view: SkillView,
    pub(crate) added: bool,
}

pub(crate) enum ResultValue {
    Skill(Box<SkillChange>),
    SkillRemoved(PathBuf),
    Server(Box<ServerView>),
    Removed(Vec<String>),
    Skills(Vec<SkillView>, Option<u16>),
    Servers(Vec<ServerView>, Option<u16>),
    Secret(Box<ServerView>, Option<String>),
    Check(Box<ServerSpec>, Vec<EffectiveSecret>),
    Effective(Box<EffectivePlugins>),
    Launch(Box<LaunchPlugins>),
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

/// Creates the skill and MCP server tables when the engine database opens.
/// Input: the engine connection. Output: any SQLite failure.
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
        assert!(!part.is_empty() && !part.contains('/') && *part != "..");
        path.push(part);
        private_directory(&path)?;
    }
    Ok(path)
}

const SECRET_BYTES_MAX: usize = 8192;

fn valid_reference(reference: &str) -> bool {
    reference.strip_prefix("secret-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

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
pub(crate) fn store_secret(
    root: &Path,
    reference: &str,
    mut secret: Vec<u8>,
) -> Result<(), EngineError> {
    let stored = (|| {
        if !valid_reference(reference) || secret.is_empty() || secret.len() > SECRET_BYTES_MAX {
            return Err(error("server_secret_invalid"));
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

fn skill_view(record: &SkillRecord, root: &Path, on_here: Option<bool>) -> SkillView {
    SkillView {
        id: record.id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        scope: record.scope.clone(),
        scope_label: record.scope.label().into(),
        enabled: record.enabled,
        on_here,
        source: record.source.clone(),
        folder: root.join(&record.folder).display().to_string(),
    }
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

fn server_view(record: &ServerRecord, root: &Path, on_here: Option<bool>) -> ServerView {
    let status = status(record);
    let digest = record.spec.digest();
    let current = record.check.as_ref().filter(|check| check.digest == digest);
    ServerView {
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
            .map(|code| {
                crate::plugins::plain_problem(code)
                    .unwrap_or("The server can't start.")
                    .into()
            }),
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
    }
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
) -> Result<SkillRecord, EngineError> {
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
    let meta = skill_folder::copy(Path::new(&source), &staging).map_err(error)?;
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
    result
}

fn add_server(
    tx: &Transaction<'_>,
    scope: Scope,
    name: String,
    spec: ServerSpec,
) -> Result<ServerRecord, EngineError> {
    crate::plugins::validate_server_name(&name)?;
    spec.validate()?;
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
        secrets: Vec::new(),
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
        if !shadowed {
            plugins.skills.push(EffectiveSkill {
                name: skill.name.clone(),
                folder: root.join(&skill.folder),
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

fn launch(root: &Path, plugins: EffectivePlugins) -> Result<LaunchPlugins, EngineError> {
    let mut servers = Vec::with_capacity(plugins.servers.len());
    for server in plugins.servers {
        let mut secrets = Vec::with_capacity(server.secrets.len());
        for secret in server.secrets {
            secrets.push(LaunchSecret {
                name: secret.name,
                value: SecretValue::new(read_secret(root, &secret.reference)?),
            });
        }
        servers.push(LaunchServer {
            name: server.name,
            spec: server.spec,
            secrets,
        });
    }
    assert!(servers.len() <= 2 * SERVERS_PER_SCOPE_MAX);
    Ok(LaunchPlugins {
        skills: plugins.skills,
        servers,
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
    let tx = connection.transaction().map_err(error)?;
    let value = run(&tx, root, command)?;
    let committed = tx.commit().map_err(error);
    match (&value, committed) {
        (ResultValue::Skill(skill), Err(problem)) if skill.added => {
            remove_folder(Path::new(&skill.view.folder));
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

fn run(tx: &Transaction<'_>, root: &Path, command: Command) -> Result<ResultValue, EngineError> {
    Ok(match command {
        Command::AddSkill { scope, source } => {
            let record = add_skill(tx, root, scope, source)?;
            ResultValue::Skill(Box::new(SkillChange {
                view: skill_view(&record, root, None),
                added: true,
            }))
        }
        Command::SetSkill { id, enabled } => {
            let mut record: SkillRecord = load(tx, "skill", &id)?;
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
                view: skill_view(&record, root, None),
                added: false,
            }))
        }
        Command::RemoveSkill { id } => {
            let record: SkillRecord = load(tx, "skill", &id)?;
            tx.execute("DELETE FROM plugin_items WHERE id=?1", [&id])
                .map_err(error)?;
            tx.execute("DELETE FROM plugin_switches WHERE item=?1", [&id])
                .map_err(error)?;
            ResultValue::SkillRemoved(root.join(&record.folder))
        }
        Command::AddServer { scope, name, spec } => {
            let record = add_server(tx, scope, name, spec)?;
            ResultValue::Server(Box::new(server_view(&record, root, None)))
        }
        Command::UpdateServer { id, spec } => {
            spec.validate()?;
            let mut record: ServerRecord = load(tx, "server", &id)?;
            if record.spec != spec {
                record.spec = spec;
                record.approved = None;
                record.check = None;
                put_server(tx, &record)?;
            }
            ResultValue::Server(Box::new(server_view(&record, root, None)))
        }
        Command::Approve { id, spec } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            if record.spec != spec {
                return Err(error("server_changed"));
            }
            record.approved = Some(record.spec.digest());
            put_server(tx, &record)?;
            ResultValue::Server(Box::new(server_view(&record, root, None)))
        }
        Command::SetServer { id, enabled } => {
            let mut record: ServerRecord = load(tx, "server", &id)?;
            record.enabled = enabled;
            put_server(tx, &record)?;
            ResultValue::Server(Box::new(server_view(&record, root, None)))
        }
        Command::RemoveServer { id } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            tx.execute("DELETE FROM plugin_items WHERE id=?1", [&id])
                .map_err(error)?;
            tx.execute("DELETE FROM plugin_switches WHERE item=?1", [&id])
                .map_err(error)?;
            ResultValue::Removed(record.secrets.into_iter().map(|s| s.reference).collect())
        }
        Command::Switch { project, id, on } => {
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
            ResultValue::Secret(Box::new(server_view(&record, root, None)), replaced)
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
            ResultValue::Secret(
                Box::new(server_view(&record, root, None)),
                Some(removed.reference),
            )
        }
        Command::Server { id } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            ResultValue::Server(Box::new(server_view(&record, root, None)))
        }
        Command::GetServer { id } => {
            let record: ServerRecord = load(tx, "server", &id)?;
            ResultValue::Check(Box::new(record.spec), record.secrets)
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
            ResultValue::Server(Box::new(server_view(&record, root, None)))
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
                        record,
                        root,
                        on_here(&record.scope, &record.id, project.as_ref(), &off),
                    )
                })
                .collect();
            let (views, next) = page(views, offset)?;
            ResultValue::Skills(views, next)
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
                        record,
                        root,
                        on_here(&record.scope, &record.id, project.as_ref(), &off),
                    )
                })
                .collect();
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
            if !project_exists(tx, &project)? {
                return Err(error("plugin_project_unknown"));
            }
            ResultValue::Launch(Box::new(launch(root, effective(tx, root, &project)?)?))
        }
    })
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
