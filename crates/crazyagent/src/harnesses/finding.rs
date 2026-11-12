use super::{Search, is_shim, is_shim_folder};
use crate::engine::EngineError;
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
#[cfg(target_os = "macos")]
const AGENT_TMP: &str = "/private/tmp/ca-";
#[cfg(not(target_os = "macos"))]
const AGENT_TMP: &str = "/tmp/ca-";

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
/// that no longer passes [`check`] is left out with a line in the engine log, so the
/// search runs instead. Inputs: the engine folder and HOME. Output: the codex and claude
/// paths; rcli is never chosen.
pub fn at_start(root: &Path, home: Option<&Path>) -> crate::paths::Harnesses {
    let read = || -> Option<Connection> {
        let db = Connection::open_with_flags(
            root.join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        db.busy_timeout(std::time::Duration::from_millis(500))
            .ok()?;
        Some(db)
    };
    let Some(db) = read() else {
        return crate::paths::Harnesses::default();
    };
    let mut kept = CHOOSABLE.map(|id| {
        let path = chosen(&db, id).ok().flatten()?;
        match check(id, &path, root, home) {
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
/// file, no version-manager shim, outside the engine folder that holds every agent home and
/// the agents' temp folders, and in folders no other user can change.
/// Inputs: the id, the path, the engine folder and HOME. Output: the path, links kept.
pub fn check(
    id: &str,
    path: &Path,
    root: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, EngineError> {
    if !CHOOSABLE.contains(&id) {
        return Err(failure(UNKNOWN_HARNESS));
    }
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
    let engine = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let in_agent_tmp = [path, real.as_path()].iter().any(|candidate| {
        candidate
            .to_str()
            .is_some_and(|text| text.starts_with(AGENT_TMP))
    });
    if path.starts_with(root) || real.starts_with(&engine) || in_agent_tmp {
        return Err(failure(PATH_AGENT_SPACE));
    }
    if crate::service::changeable_by_others(path).is_some() {
        return Err(failure(PATH_CHANGEABLE));
    }
    assert!(path.is_absolute());
    Ok(path.to_path_buf())
}

/// Picks the file a pin may use from what a search finds: the first candidate, in search
/// order and then on PATH, that passes [`check`], so a shim or a file in a folder another
/// user can change never hides a good install after it. Inputs: the id, the search, the PATH
/// value and the engine folder. Output: the entry, the first refusal when every candidate
/// failed, or none when nothing was found.
pub fn pick(
    id: &str,
    search: &Search,
    path: Option<&OsStr>,
    root: &Path,
) -> Option<Result<PathBuf, (PathBuf, EngineError)>> {
    let mut refused = None;
    for candidate in crate::paths::harness_candidates(id, search, path) {
        match check(id, &candidate, root, search.home()) {
            Ok(entry) => return Some(Ok(entry)),
            Err(error) => {
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
/// `~`, a shim folder marked "(shim, skipped)", at most [`LOOKED_MAX`] folders, each at most
/// [`LOOKED_BYTES_MAX`] bytes, so the answer always fits one reply frame. Inputs: the search
/// and the id. Output: the lines.
pub fn looked(search: &Search, id: &str) -> Vec<String> {
    let home = search.home();
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
            }
            (shown.len() <= LOOKED_BYTES_MAX).then_some(shown)
        })
        .take(LOOKED_MAX)
        .collect();
    assert!(lines.len() <= LOOKED_MAX);
    lines
}
