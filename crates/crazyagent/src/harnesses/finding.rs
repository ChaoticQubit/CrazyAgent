use super::{Search, in_crazyagent_folder, is_shim, is_shim_folder};
use crate::engine::EngineError;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The harnesses whose file the owner may choose: the two with a managed sign-in.
pub const CHOOSABLE: [&str; 2] = ["codex", "claude-code"];
pub const PATH_BYTES_MAX: usize = 1024;
pub const UNKNOWN_HARNESS: &str = "harness_path_unknown_harness";
pub const PATH_INVALID: &str = "harness_path_invalid";
pub const PATH_NOT_FOUND: &str = "harness_path_not_found";
pub const PATH_NOT_EXECUTABLE: &str = "harness_path_not_executable";
pub const PATH_AGENT_SPACE: &str = "harness_path_agent_space";
pub const PATH_SHIM: &str = "harness_path_shim";
pub const PATH_CHANGEABLE: &str = "harness_path_changeable";
pub const PATH_UNREADABLE: &str = "harness_path_unreadable";
/// The most folders a "where we looked" answer names, and the longest one shown.
pub const LOOKED_MAX: usize = 48;
pub const LOOKED_BYTES_MAX: usize = 256;
const SHIM_SKIPPED: &str = " (shim, skipped)";
/// The mark on a folder the engine's search passed over for a temp folder, in the words the
/// tool resolver uses for the same refusal.
pub const TEMP_SKIPPED: &str =
    " (skipped: in a temp folder or links to one, which agents or other users can change)";
#[cfg(target_os = "macos")]
const AGENT_TMP: &str = "/private/tmp/ca-";
#[cfg(not(target_os = "macos"))]
const AGENT_TMP: &str = "/tmp/ca-";
/// The most rows read from one table that names folders agents can write, and the most
/// repositories one project may declare; more is refused as unreadable, so no folder is
/// ever left out of an [`AgentSpace`].
pub const AGENT_ROWS_MAX: usize = 1024;
pub const PROJECT_REPOSITORIES_MAX: usize = 64;
const AGENT_FOLDERS_MAX: usize = 2 * (AGENT_ROWS_MAX * (PROJECT_REPOSITORIES_MAX + 3) + 1);
const AGENT_TABLES: [(&str, Names, &str); 4] = [
    (
        "project_setups",
        Names::Setup,
        "SELECT body FROM project_setups LIMIT ?1",
    ),
    (
        "projects",
        Names::Project,
        "SELECT body FROM projects LIMIT ?1",
    ),
    (
        "team_worktrees",
        Names::Path,
        "SELECT repository FROM team_worktrees UNION SELECT worktree FROM team_worktrees
         WHERE instr(worktree, '/.crazyagent/') = 0
         AND substr(worktree, 1, length(repository) + 1) <> (repository || '/') LIMIT ?1",
    ),
    (
        "worktree_leases",
        Names::Path,
        "SELECT repository FROM worktree_leases UNION SELECT path FROM worktree_leases LIMIT ?1",
    ),
];

/// How one engine table names a folder agents can write: a setup body with the project
/// folder, a project body with the repositories it declares, or a stored path. An agent
/// worktree inside its repository or a `.crazyagent` folder is covered already and not read,
/// so kept agents never fill [`AGENT_ROWS_MAX`].
#[derive(Debug, Clone, Copy)]
enum Names {
    Setup,
    Project,
    Path,
}

/// The folders agents can write, where no harness file may come from: the engine folder
/// outside its `setup` folder ([`crate::tools::in_engine_data`]) with every agent home, the
/// agents' temp folders, any `.crazyagent` folder with its agent worktrees, and each project
/// folder, declared repository and agent worktree the engine database names, each kept as
/// stored and as its real path. A folder holding the owner's home is left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpace {
    engine: Vec<PathBuf>,
    folders: Vec<PathBuf>,
}

impl AgentSpace {
    /// Reads the agent space of an engine folder whose database may not exist yet: none
    /// means no project yet. Input: the engine folder. Output: the space, or
    /// `harness_path_unreadable` when the database is there but cannot be read.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        match std::fs::symlink_metadata(root.join("engine.db")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::of(root, Vec::new()));
            }
            Err(_) => return Err(failure(PATH_UNREADABLE)),
            Ok(_) => {}
        }
        let db = engine_db(root).ok_or_else(|| failure(PATH_UNREADABLE))?;
        Self::read(root, &db)
    }

    /// Reads the agent space from the engine database; a table not made yet names nothing.
    /// Inputs: the engine folder and its database. Output: the space, or
    /// `harness_path_unreadable` for a failed read, a body that is not JSON, or more rows
    /// than [`AGENT_ROWS_MAX`], so a read that falls short never lets a file through.
    pub fn read(root: &Path, db: &Connection) -> Result<Self, EngineError> {
        let unreadable = |_: rusqlite::Error| failure(PATH_UNREADABLE);
        let mut named = Vec::new();
        for (table, names, query) in AGENT_TABLES {
            let present: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .map_err(unreadable)?;
            if !present {
                continue;
            }
            let mut statement = db.prepare(query).map_err(unreadable)?;
            let rows: Vec<String> = statement
                .query_map([AGENT_ROWS_MAX as i64 + 1], |row| row.get(0))
                .map_err(unreadable)?
                .collect::<Result<_, _>>()
                .map_err(unreadable)?;
            if rows.len() > AGENT_ROWS_MAX {
                return Err(failure(PATH_UNREADABLE));
            }
            for text in rows {
                named.extend(folders_in(names, &text)?);
            }
        }
        Ok(Self::of(root, named))
    }

    /// Builds the space from the engine folder and the folders the database names.
    /// Inputs: the engine folder and the named folders. Output: the space, each absolute
    /// folder kept as named and as its real path, none that holds the owner's home.
    fn of(root: &Path, named: Vec<PathBuf>) -> Self {
        let owner = crate::projects_home::owner_home().ok();
        let usable = |folder: &PathBuf| {
            folder.is_absolute()
                && folder.parent().is_some()
                && !owner
                    .as_deref()
                    .is_some_and(|owner| owner.starts_with(folder))
        };
        let engine: Vec<PathBuf> = crate::tools::engine_roots(Some(root))
            .into_iter()
            .filter(usable)
            .collect();
        let mut folders = Vec::new();
        for folder in named {
            let real = std::fs::canonicalize(&folder).ok();
            folders.extend(std::iter::once(folder).chain(real).filter(usable));
        }
        folders.sort();
        folders.dedup();
        assert!(engine.len() <= 2);
        assert!(folders.len() <= AGENT_FOLDERS_MAX);
        Self { engine, folders }
    }

    /// Whether agents could write a harness file or the link to it: the path as given, the
    /// link in its real folder, or its real file lies in the space. Input: an absolute path.
    /// Output: the answer.
    pub fn holds(&self, path: &Path) -> bool {
        let placed = path
            .parent()
            .and_then(|folder| std::fs::canonicalize(folder).ok())
            .zip(path.file_name())
            .map(|(folder, name)| folder.join(name));
        let real = std::fs::canonicalize(path).ok();
        [Some(path.to_path_buf()), placed, real]
            .into_iter()
            .flatten()
            .any(|at| {
                in_crazyagent_folder(&at)
                    || at.to_str().is_some_and(|text| text.starts_with(AGENT_TMP))
                    || crate::tools::in_engine_data(&self.engine, &at)
                    || self.folders.iter().any(|folder| at.starts_with(folder))
            })
    }
}

/// The folders one engine row names. Inputs: how the table names them and the row text.
/// Output: a setup's project folder, the repositories a project declares, or the stored
/// path; `harness_path_unreadable` for a body that is not JSON or declares more than
/// [`PROJECT_REPOSITORIES_MAX`] repositories.
fn folders_in(names: Names, text: &str) -> Result<Vec<PathBuf>, EngineError> {
    let body = || -> Result<serde_json::Value, EngineError> {
        serde_json::from_str(text).map_err(|_| failure(PATH_UNREADABLE))
    };
    let folders: Vec<PathBuf> = match names {
        Names::Path => vec![PathBuf::from(text)],
        Names::Setup => body()?
            .get("folder")
            .and_then(serde_json::Value::as_str)
            .map(|folder| {
                let folder = Path::new(folder);
                match folder.parent() {
                    Some(project) if folder.ends_with(CRAZYAGENT_DIRECTORY) => {
                        project.to_path_buf()
                    }
                    _ => folder.to_path_buf(),
                }
            })
            .into_iter()
            .collect(),
        Names::Project => {
            let body = body()?;
            let repositories = body
                .pointer("/spec/repositories")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if repositories.len() > PROJECT_REPOSITORIES_MAX {
                return Err(failure(PATH_UNREADABLE));
            }
            repositories
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(PathBuf::from)
                .collect()
        }
    };
    assert!(folders.len() <= PROJECT_REPOSITORIES_MAX);
    Ok(folders)
}

/// Opens the engine database without making it, as engine start reads it before the
/// engine opens it. Input: the engine folder. Output: the connection, or none when there
/// is no database or it cannot be opened.
fn engine_db(root: &Path) -> Option<Connection> {
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.busy_timeout(std::time::Duration::from_millis(500))
        .ok()?;
    Some(db)
}

const TABLE: &str = "CREATE TABLE IF NOT EXISTS harness_chosen_paths (
    id TEXT PRIMARY KEY CHECK(id IN ('codex', 'claude-code')),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 2 AND 1024)
)";

/// What looking for one harness found, as Check again, Choose file and the sign-in retry
/// answer it: pinned now, pinned before, found nowhere with the folders looked in, or found
/// but refused with the plain code of the check it failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Found {
    Registered { path: PathBuf },
    Ready { path: PathBuf },
    Missing { looked: Vec<String> },
    Refused { path: PathBuf, code: String },
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Creates the chosen harness file table when it is missing.
/// Input: the engine database. Output: Ok, or `harness_path_unreadable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(TABLE)
        .map_err(|_| failure(PATH_UNREADABLE))
}

/// Reads the file the owner chose for one harness. Inputs: the engine database with the
/// table in place and the catalog id. Output: the path, none when nothing was chosen, or
/// `harness_path_unreadable`.
pub fn chosen(db: &Connection, id: &str) -> Result<Option<PathBuf>, EngineError> {
    if !CHOOSABLE.contains(&id) {
        return Err(failure(UNKNOWN_HARNESS));
    }
    let stored: Option<String> = db
        .query_row(
            "SELECT path FROM harness_chosen_paths WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure(PATH_UNREADABLE))?;
    Ok(stored.map(PathBuf::from).filter(|path| path.is_absolute()))
}

/// Stores or clears the file the owner chose for one harness. Inputs: the engine database
/// with the table in place, the catalog id and the checked path, none to clear it.
/// Output: Ok once stored, or `harness_path_unreadable`.
pub fn set_chosen(db: &Connection, id: &str, path: Option<&Path>) -> Result<(), EngineError> {
    if !CHOOSABLE.contains(&id) {
        return Err(failure(UNKNOWN_HARNESS));
    }
    let Some(path) = path else {
        db.execute("DELETE FROM harness_chosen_paths WHERE id=?1", [id])
            .map_err(|_| failure(PATH_UNREADABLE))?;
        return Ok(());
    };
    let text = path.to_str().ok_or_else(|| failure(PATH_INVALID))?;
    assert!(path.is_absolute() && text.len() <= PATH_BYTES_MAX);
    let stored = db
        .execute(
            "INSERT INTO harness_chosen_paths(id,path) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET path=excluded.path",
            rusqlite::params![id, text],
        )
        .map_err(|_| failure(PATH_UNREADABLE))?;
    assert_eq!(stored, 1);
    Ok(())
}

/// The files the owner chose, read once at engine start before the engine opens its
/// database: an engine folder with no database or no table has none, and a chosen file
/// that no longer passes [`check`], or whose [`AgentSpace`] cannot be read, is left out
/// with a line in the engine log, so the search runs instead. Inputs: the engine folder
/// and HOME. Output: the codex and claude paths; rcli is never chosen.
pub fn at_start(root: &Path, home: Option<&Path>) -> crate::paths::Harnesses {
    let Some(db) = engine_db(root) else {
        return crate::paths::Harnesses::default();
    };
    let space = match AgentSpace::read(root, &db) {
        Ok(space) => space,
        Err(error) => {
            eprintln!("the chosen harness files are not used ({})", error.0);
            return crate::paths::Harnesses::default();
        }
    };
    let mut kept = CHOOSABLE.map(|id| {
        let path = chosen(&db, id).ok().flatten()?;
        match check(id, &path, &space, home) {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("the chosen {id} file is not used ({})", error.0);
                None
            }
        }
    });
    crate::paths::Harnesses {
        codex: kept[0].take(),
        claude: kept[1].take(),
        rcli: None,
    }
}

/// Checks a harness file, chosen or found, before it is pinned or stored: an absolute path
/// of at most [`PATH_BYTES_MAX`] bytes with no control characters, to a runnable regular
/// file, no version-manager shim, outside every folder agents can write ([`AgentSpace`]),
/// in folders no other user can change, whose real file passes the pin's own file rule.
/// Inputs: the id, the path, the agent space and HOME. Output: the path, links kept.
pub fn check(
    id: &str,
    path: &Path,
    space: &AgentSpace,
    home: Option<&Path>,
) -> Result<PathBuf, EngineError> {
    if !CHOOSABLE.contains(&id) {
        return Err(failure(UNKNOWN_HARNESS));
    }
    check_file(path, space, home)
}

/// Checks a harness file by the rules of [`check`] whatever its id, so a pinned rcli or a
/// harness a PATH entry leads to again later passes the same rules as at engine start.
/// Inputs: the path, the agent space and HOME. Output: the path, links kept, or the refusal.
pub fn check_file(
    path: &Path,
    space: &AgentSpace,
    home: Option<&Path>,
) -> Result<PathBuf, EngineError> {
    let text = path.to_str().ok_or_else(|| failure(PATH_INVALID))?;
    if !path.is_absolute()
        || text.len() > PATH_BYTES_MAX
        || text.chars().any(char::is_control)
        || path.parent().is_none()
    {
        return Err(failure(PATH_INVALID));
    }
    let real = std::fs::canonicalize(path).map_err(|_| failure(PATH_NOT_FOUND))?;
    let metadata = std::fs::metadata(&real).map_err(|_| failure(PATH_NOT_FOUND))?;
    if !metadata.is_file() || !owner_runnable(&metadata) {
        return Err(failure(PATH_NOT_EXECUTABLE));
    }
    if is_shim(home, path) {
        return Err(failure(PATH_SHIM));
    }
    if space.holds(path) {
        return Err(failure(PATH_AGENT_SPACE));
    }
    if crate::service::changeable_by_others(path).is_some() {
        return Err(failure(PATH_CHANGEABLE));
    }
    crate::engine::pinnable(&metadata)?;
    assert!(path.is_absolute());
    Ok(path.to_path_buf())
}

/// Picks the file a pin may use from what a search finds: the first candidate, in search
/// order and then on PATH, that passes [`check`], so a shim, a file the pin refuses or one
/// in a folder another user can change never hides a good install after it. Inputs: the id,
/// the search, the PATH value and the agent space. Output: the entry, the first refusal when
/// every candidate failed, or none when nothing was found.
pub fn pick(
    id: &str,
    search: &Search,
    path: Option<&OsStr>,
    space: &AgentSpace,
) -> Option<Result<PathBuf, (PathBuf, EngineError)>> {
    pick_noting(id, search, path, space, |_, _| {})
}

/// Picks as [`pick`] does and hands each candidate it passes over, with the refusal, to
/// `passed`, so engine start names every found file it does not use, even when a later one
/// is used. Inputs: as for [`pick`], and the callback. Output: as for [`pick`].
pub fn pick_noting(
    id: &str,
    search: &Search,
    path: Option<&OsStr>,
    space: &AgentSpace,
    mut passed: impl FnMut(&Path, &EngineError),
) -> Option<Result<PathBuf, (PathBuf, EngineError)>> {
    let mut refused = None;
    for candidate in crate::paths::harness_candidates(id, search, path) {
        match check(id, &candidate, space, search.home()) {
            Ok(entry) => return Some(Ok(entry)),
            Err(error) => {
                passed(&candidate, &error);
                refused.get_or_insert((candidate, error));
            }
        }
    }
    refused.map(Err)
}

#[cfg(unix)]
fn owner_runnable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn owner_runnable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// The folders a search looked in for one harness as the owner reads them: HOME shown as
/// `~`, a shim folder marked "(shim, skipped)", a folder passed over for a temp folder marked
/// with that reason ([`TEMP_SKIPPED`]), at most [`LOOKED_MAX`] folders, each at most
/// [`LOOKED_BYTES_MAX`] bytes, so the answer always fits one reply frame. Inputs: the search
/// and the id. Output: the lines.
pub fn looked(search: &Search, id: &str) -> Vec<String> {
    let home = search.home();
    let skipped = search.temp_skipped(id);
    let lines: Vec<String> = search
        .looked(id)
        .into_iter()
        .filter_map(|folder| {
            let mut shown = match home.and_then(|home| folder.strip_prefix(home).ok()) {
                Some(rest) => format!("~/{}", rest.display()),
                None => folder.display().to_string(),
            };
            if is_shim_folder(home, &folder) {
                shown.push_str(SHIM_SKIPPED);
            } else if skipped.contains(&folder) {
                shown.push_str(TEMP_SKIPPED);
            }
            (shown.len() <= LOOKED_BYTES_MAX).then_some(shown)
        })
        .take(LOOKED_MAX)
        .collect();
    assert!(lines.len() <= LOOKED_MAX);
    lines
}
