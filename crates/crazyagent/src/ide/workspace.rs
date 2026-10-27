use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
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

/// The workspace settings: `readonly_include` holds the globs the IDE opens read only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceSettings {
    pub readonly_include: Vec<String>,
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
        .prepare_cached("SELECT project FROM ide_workspaces ORDER BY project LIMIT ?1")
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
    let body = render(&sections, &WorkspaceSettings::default())?;
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
    let folder = {
        let tx = db.unchecked_transaction().map_err(unavailable)?;
        crate::project_setup::load_setup(&tx, project)?
            .ok_or_else(|| failure("ide_project_unknown"))?
            .project_folder()
    };
    if !folder.is_absolute() {
        return Err(failure("ide_project_folder_invalid"));
    }
    let mut statement = db.prepare_cached(AGENTS).map_err(unavailable)?;
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

/// Renders the workspace file: its folders in order, its settings and the keys that keep the
/// IDE's git to each section's root. Inputs: the sections and settings. Output: the JSON text.
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
    let readonly: serde_json::Map<String, serde_json::Value> = settings
        .readonly_include
        .iter()
        .map(|glob| (glob.clone(), serde_json::Value::Bool(true)))
        .collect();
    let mut keys = super::git::workspace();
    keys.insert(
        "files.readonlyInclude".into(),
        serde_json::Value::Object(readonly),
    );
    let body = serde_json::json!({ "folders": folders, "settings": keys });
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

fn unwritable(_: std::io::Error) -> EngineError {
    failure("ide_workspace_unwritable")
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
