use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::sandbox::policy::{AgentAccess, AgentNetwork};
use crate::team::AgentRole;
use crate::team::names::{self, AgentName};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The most agent sections a workspace lists, newest first, until the owner shows all.
pub const AGENTS_SHOWN_MAX: usize = 12;
/// The most agent sections a workspace lists with every agent shown.
pub const AGENTS_LISTED_MAX: usize = 256;
/// The generated workspace file inside the project's IDE folder.
pub const FILE: &str = "crazyagent.code-workspace";

/// The most entries of a project's older agents root read for research worktrees; a root
/// holding more counts as holding one, so the git stop is kept rather than guessed away.
pub(crate) const OLDER_ENTRIES_MAX: usize = 4096;

#[cfg(unix)]
const TEMPORARY: &str = "crazyagent.code-workspace.tmp";
const PROJECTS_FOLLOWED_MAX: i64 = 64;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ide_workspaces (
    project TEXT PRIMARY KEY,
    show_all INTEGER NOT NULL CHECK(show_all IN (0,1)),
    body TEXT NOT NULL
);";
const AGENTS: &str = "SELECT a.id, a.role, w.worktree, n.persona, n.handle
    FROM team_agents a
    JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
    LEFT JOIN team_names n ON n.agent=a.id
    WHERE a.project=?1
      AND (a.status IN ('starting','working')
           OR (a.role='master' AND a.status='idle')
           OR EXISTS (SELECT 1 FROM landing_change_sets c
                      WHERE c.project=?1 AND c.worktree=w.worktree
                        AND c.state IN ('pending','conflict','updating')))
    ORDER BY a.created_ms DESC, a.id DESC
    LIMIT ?2";

static WRITING: Mutex<()> = Mutex::new(());

const _: () = assert!(AGENTS_SHOWN_MAX < AGENTS_LISTED_MAX);

/// One workspace folder: the name the IDE shows and the folder it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub path: PathBuf,
}

/// The workspace settings: `readonly_include` holds the globs the IDE opens read only and
/// `readonly_exclude` the globs it opens writable even inside those.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceSettings {
    pub readonly_include: Vec<String>,
    pub readonly_exclude: Vec<String>,
}

/// A written workspace: its file, how many agents the cap left out and whether every
/// agent is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    pub hidden: usize,
    pub show_all: bool,
}

/// Writes the project's workspace file `<root>/ide/<project>/crazyagent.code-workspace`
/// and keeps it followed, so `follow` rewrites it when its sections change.
/// Inputs: the engine root holding `engine.db` and the project. Output: the workspace.
pub fn write(root: &Path, project: &ProjectId) -> Result<Written, EngineError> {
    let db = open(root)?;
    refresh(&db, root, project, None, true)
}

/// Sets whether the project's workspace lists every agent or the newest
/// `AGENTS_SHOWN_MAX`, then writes it. Inputs: the engine root, project and switch.
/// Output: the workspace.
pub fn show_all(root: &Path, project: &ProjectId, on: bool) -> Result<Written, EngineError> {
    let db = open(root)?;
    refresh(&db, root, project, Some(on), true)
}

/// Rewrites each followed workspace whose sections changed since it was written, and stops
/// following projects that no longer exist. The team runtime calls it every step.
/// Inputs: the engine database and root. Output: the first failure after trying them all.
pub fn follow(db: &Connection, root: &Path) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let mut statement = db
        .prepare("SELECT project FROM ide_workspaces ORDER BY project LIMIT ?1")
        .map_err(unavailable)?;
    let followed: Vec<String> = statement
        .query_map([PROJECTS_FOLLOWED_MAX], |row| row.get(0))
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    let mut first = Ok(());
    for project in followed {
        let outcome = ProjectId::new(&project)
            .map_err(unavailable)
            .and_then(|id| refresh(db, root, &id, None, false));
        match outcome {
            Ok(_) => {}
            Err(error) if error.0 == "ide_project_unknown" => {
                db.execute("DELETE FROM ide_workspaces WHERE project=?1", [&project])
                    .map_err(unavailable)?;
            }
            Err(error) => first = first.and(Err(error)),
        }
    }
    first
}

/// Lists the folders the project's workspace shows, with the owner's stored show-all
/// switch, in workspace order with "Project" first. Inputs: the engine database and the
/// project. Output: the sections, or `ide_project_unknown`.
pub fn listed(db: &Connection, project: &ProjectId) -> Result<Vec<Section>, EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let show_all: bool = db
        .query_row(
            "SELECT show_all FROM ide_workspaces WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .unwrap_or(false);
    let (sections, _) = sections(db, project, show_all)?;
    Ok(sections)
}

/// The project folder the IDE server opens and its sandbox grants: the stored folder with
/// every link resolved. Inputs: the engine root and the project. Output: the folder, or
/// `ide_project_unknown` or `ide_project_folder_invalid`.
pub fn folder(root: &Path, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let db = open(root)?;
    let folder = project_folder(&db, project)?;
    crate::sandbox::canonical(&folder).map_err(|_| failure("ide_project_folder_invalid"))
}

/// Whether git in the project's IDE runs the project's own hooks in its Project section, as
/// the owner saved it in the project's agent policy; Off when none is saved. Inputs: the
/// engine root and the project. Output: the switch, or `ide_workspace_unavailable` for a
/// policy that cannot be read, so a tampered one never turns hooks on.
pub fn project_hooks(root: &Path, project: &ProjectId) -> Result<bool, EngineError> {
    let db = open(root)?;
    crate::sandbox::policy::load(&db, project.as_str())
        .map(|policy| policy.ide_git_hooks)
        .map_err(unavailable)
}

/// What a project's IDE server starts from, as the owner saved it in the project's agent
/// policy: the access level and the network choice an Own branch IDE follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdePolicy {
    pub access: AgentAccess,
    pub network: AgentNetwork,
}

impl IdePolicy {
    /// Tells whether a server launched under this policy still honours the saved one: the
    /// same level, and at Own branch the same network choice, which no other level reads.
    /// Input: the saved policy. Output: true when the server may keep running.
    pub fn holds(self, saved: Self) -> bool {
        self.access == saved.access
            && (saved.access != AgentAccess::OwnBranch || self.network == saved.network)
    }
}

/// The loopback ports of the owner's configured local model endpoints, read from the engine's
/// settings files at each launch, for a sandbox to keep free of the listeners of what it runs.
/// Input: the two settings files, or none for an engine that has none. Output: the ports, or
/// `ide_settings_unavailable` for files that do not read, so a refused read stops the launch.
pub fn kept_ports(settings: Option<&(PathBuf, PathBuf)>) -> Result<Vec<u16>, EngineError> {
    match settings {
        None => Ok(Vec::new()),
        Some(_) => Err(failure("ide_settings_stub")),
    }
}

/// A project's [`IdePolicy`]; the default, Sandboxed with the Open network no Sandboxed IDE
/// reads, when none is saved. Inputs: the engine root and the project. Output: the policy, or
/// `ide_workspace_unavailable` for one that cannot be read, so a tampered one never widens
/// the IDE.
pub fn ide_policy(root: &Path, project: &ProjectId) -> Result<IdePolicy, EngineError> {
    let db = open(root)?;
    crate::sandbox::policy::load(&db, project.as_str())
        .map(|policy| IdePolicy {
            access: policy.access,
            network: policy.network,
        })
        .map_err(unavailable)
}

/// Whether an engine-made worktree of the project is inside the older agents root
/// `<folder>/worktrees`, where agents and research jobs that ran before the project gained a
/// `.crazyagent` folder stay: a team worktree the engine records there, live or kept
/// (INT1-R3-1) or waiting in `team_retiring` after a cleanup cut off between its claim and
/// the removal (CEIL-L1), or a research job's worktree ([`older_research`], INT1R3-L1).
/// Inputs: the engine root, the project and its canonical folder. Output: the answer, or
/// `ide_project_folder_invalid` for a folder that is not absolute text, or
/// `ide_workspace_unavailable` when the records or the folder cannot be read.
pub fn older_agents(root: &Path, project: &ProjectId, folder: &Path) -> Result<bool, EngineError> {
    if !folder.is_absolute() {
        return Err(failure("ide_project_folder_invalid"));
    }
    let older = folder.join("worktrees");
    let mut prefix = older
        .clone()
        .into_os_string()
        .into_string()
        .map_err(|_| failure("ide_project_folder_invalid"))?;
    prefix.push('/');
    assert!(prefix.starts_with('/') && prefix.ends_with("/worktrees/"));
    let db = open(root)?;
    crate::team::initialize(&db).map_err(unavailable)?;
    crate::team::cleanup::initialize(&db).map_err(unavailable)?;
    let team: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_worktrees
                           WHERE project=?1 AND substr(worktree,1,length(?2))=?2)
                 OR EXISTS(SELECT 1 FROM team_retiring
                           WHERE project=?1 AND substr(worktree,1,length(?2))=?2)",
            params![project.as_str(), prefix],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok(team || older_research(&db, project, &older)?)
}

/// Whether a research job of the project left a worktree in the older agents root: a real
/// folder there whose name starts `research-<kind>-<job id>-`, as the research runner names
/// each launch's worktree, for a job the engine records for this project. Research
/// worktrees have no team record and stay after their job. Inputs: the database, the project
/// and `<folder>/worktrees`. Output: the answer, true for a root of more than
/// [`OLDER_ENTRIES_MAX`] entries, or `ide_workspace_unavailable` when it cannot be read.
fn older_research(db: &Connection, project: &ProjectId, older: &Path) -> Result<bool, EngineError> {
    let recorded: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='research_jobs')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !recorded {
        return Ok(false);
    }
    let entries = match std::fs::symlink_metadata(older) {
        Ok(metadata) if metadata.is_dir() => std::fs::read_dir(older).map_err(unavailable)?,
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(unavailable(error)),
    };
    let mut job = db
        .prepare(
            "SELECT EXISTS(SELECT 1 FROM research_jobs WHERE project=?1
                AND substr(?2,1,length('research-'||kind||'-'||id||'-'))='research-'||kind||'-'||id||'-')",
        )
        .map_err(unavailable)?;
    for (read, entry) in entries.enumerate() {
        if read == OLDER_ENTRIES_MAX {
            return Ok(true);
        }
        let entry = entry.map_err(unavailable)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with("research-") || !entry.file_type().map_err(unavailable)?.is_dir() {
            continue;
        }
        let made: bool = job
            .query_row(params![project.as_str(), name], |row| row.get(0))
            .map_err(unavailable)?;
        if made {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reads the project's stored folder. Inputs: the database and project. Output: the
/// absolute folder, or `ide_project_unknown` or `ide_project_folder_invalid`.
fn project_folder(db: &Connection, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let folder = {
        let tx = db.unchecked_transaction().map_err(unavailable)?;
        crate::project_setup::load_setup(&tx, project)?
            .ok_or_else(|| failure("ide_project_unknown"))?
            .project_folder()
    };
    if !folder.is_absolute() {
        return Err(failure("ide_project_folder_invalid"));
    }
    Ok(folder)
}

/// Opens the engine database with the change set and workspace tables in place.
/// Input: the engine root. Output: the connection.
fn open(root: &Path) -> Result<Connection, EngineError> {
    let db = crate::landing::open(root).map_err(unavailable)?;
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    Ok(db)
}

/// Writes the project's workspace when `always` or when its switch or sections changed,
/// one writer at a time. Inputs: the database, engine root, project, new switch (none
/// keeps the stored one) and whether to write unchanged content. Output: the workspace.
fn refresh(
    db: &Connection,
    root: &Path,
    project: &ProjectId,
    show_all: Option<bool>,
    always: bool,
) -> Result<Written, EngineError> {
    if project.as_str() == "." {
        return Err(failure("ide_project_invalid"));
    }
    let _writing = WRITING.lock().map_err(unavailable)?;
    let kept: Option<(bool, String)> = db
        .query_row(
            "SELECT show_all, body FROM ide_workspaces WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let show_all = show_all
        .or(kept.as_ref().map(|(on, _)| *on))
        .unwrap_or(false);
    let (sections, hidden) = sections(db, project, show_all)?;
    let body = render(&sections, &super::readonly::settings(db, project)?)?;
    let folder = root.join("ide").join(project.as_str());
    if always || kept.is_none_or(|(on, kept)| on != show_all || kept != body) {
        super::bridge::private_folder(&root.join("ide"))?;
        super::bridge::private_folder(&folder)?;
        replace(&folder, body.as_bytes())?;
        db.execute(
            "INSERT INTO ide_workspaces(project, show_all, body) VALUES (?1,?2,?3)
             ON CONFLICT(project) DO UPDATE SET show_all=excluded.show_all, body=excluded.body",
            params![project.as_str(), show_all, body],
        )
        .map_err(unavailable)?;
    }
    Ok(Written {
        path: folder.join(FILE),
        hidden,
        show_all,
    })
}

/// Lists the project's sections: "Project", then "master" for the newest master that is
/// live or has an open change set, then every running agent and every agent with an open
/// change set, newest first, the newest `AGENTS_SHOWN_MAX` unless all are shown. Older
/// masters are named "master (<id>)". Inputs: the database, project and switch.
/// Output: the sections and how many agents the cap left out.
fn sections(
    db: &Connection,
    project: &ProjectId,
    show_all: bool,
) -> Result<(Vec<Section>, usize), EngineError> {
    let folder = project_folder(db, project)?;
    let mut statement = db.prepare(AGENTS).map_err(unavailable)?;
    let rows = statement
        .query_map(params![project.as_str(), AGENTS_LISTED_MAX as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(unavailable)?;
    let mut master = None;
    let mut agents = Vec::new();
    for row in rows {
        let (id, role, worktree, persona, handle) = row.map_err(unavailable)?;
        let path = PathBuf::from(worktree);
        let name = match role.as_str() {
            "master" if master.is_none() => {
                master = Some(Section {
                    name: "master".into(),
                    path,
                });
                continue;
            }
            "master" => format!("master ({id})"),
            "sub" => {
                let name = persona.map(|persona| AgentName { persona, handle });
                names::label(AgentRole::Sub, id, name.as_ref())
            }
            _ => return Err(failure("ide_agent_invalid")),
        };
        agents.push(Section { name, path });
    }
    assert!(agents.len() + usize::from(master.is_some()) <= AGENTS_LISTED_MAX);
    let hidden = if show_all {
        0
    } else {
        agents.len().saturating_sub(AGENTS_SHOWN_MAX)
    };
    agents.truncate(agents.len() - hidden);
    let mut sections = vec![Section {
        name: "Project".into(),
        path: folder,
    }];
    sections.extend(master);
    sections.extend(agents);
    assert!(show_all || sections.len() <= 2 + AGENTS_SHOWN_MAX);
    assert!(sections.len() <= 1 + AGENTS_LISTED_MAX);
    Ok((sections, hidden))
}

/// Renders the workspace file: its folders in order, the lock's workspace keys, the keys
/// that keep the IDE's git to each section's root and its settings.
/// Inputs: the sections and settings. Output: the JSON text.
fn render(sections: &[Section], settings: &WorkspaceSettings) -> Result<String, EngineError> {
    let folders = sections
        .iter()
        .map(|section| -> Result<serde_json::Value, EngineError> {
            let path = section
                .path
                .to_str()
                .ok_or_else(|| failure("ide_path_unreadable"))?;
            Ok(serde_json::json!({ "name": section.name, "path": path }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let globs = |globs: &[String]| -> serde_json::Map<String, serde_json::Value> {
        globs
            .iter()
            .map(|glob| (glob.clone(), serde_json::Value::Bool(true)))
            .collect()
    };
    let mut locked = super::settings::workspace();
    locked.extend(super::git::workspace());
    locked.insert(
        "files.readonlyInclude".to_owned(),
        serde_json::Value::Object(globs(&settings.readonly_include)),
    );
    locked.insert(
        "files.readonlyExclude".to_owned(),
        serde_json::Value::Object(globs(&settings.readonly_exclude)),
    );
    let body = serde_json::json!({ "folders": folders, "settings": locked });
    serde_json::to_string_pretty(&body).map_err(unavailable)
}

/// Replaces the workspace file whole: writes a new 0600 file beside it, syncs it and
/// renames it over the old one, so the IDE never reads a partial file.
/// Inputs: the project's IDE folder and the content. Output: Ok once renamed.
#[cfg(unix)]
fn replace(folder: &Path, body: &[u8]) -> Result<(), EngineError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = folder.join(TEMPORARY);
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unwritable(error)),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(unwritable)?;
    file.write_all(body).map_err(unwritable)?;
    file.sync_all().map_err(unwritable)?;
    std::fs::rename(&temporary, folder.join(FILE)).map_err(unwritable)
}

/// Refuses workspace files where the engine has no Unix file modes.
/// Takes the folder and content, unused. Returns `ide_unavailable`.
#[cfg(not(unix))]
fn replace(_folder: &Path, _body: &[u8]) -> Result<(), EngineError> {
    Err(failure("ide_unavailable"))
}

fn unavailable<E>(_: E) -> EngineError {
    failure("ide_workspace_unavailable")
}

#[cfg(unix)]
fn unwritable(_: std::io::Error) -> EngineError {
    failure("ide_workspace_unwritable")
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
