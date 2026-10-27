use super::{canonical, clear_of_secrets, granted_root, owner_home, owner_secrets};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

/// The most extra read-only folders one project may name.
pub const EXTRA_PATHS_MAX: usize = 8;

const PATH_TEXT_MAX: usize = 1024;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_read_paths (
    project TEXT NOT NULL,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project, position)
)";

/// Checks one extra read-only folder the owner named for a project: an absolute existing
/// folder, taken in its canonical form, that neither holds the owner's home nor holds or
/// sits in a credential location, the CrazyAgent data folder or a harness home.
/// Inputs: the typed path and the engine's data folder. Output: the canonical folder, or a
/// plain refusal code.
pub fn checked(text: &str, data_root: &Path) -> Result<PathBuf, &'static str> {
    let typed = Path::new(text);
    if text.len() > PATH_TEXT_MAX || !typed.is_absolute() || text.chars().any(char::is_control) {
        return Err("read_path_not_absolute");
    }
    let real = canonical(typed).map_err(|_| "read_path_missing")?;
    if !real.is_dir() {
        return Err("read_path_not_folder");
    }
    let owner = owner_home().map_err(|_| "read_path_owner_unknown")?;
    if owner.starts_with(&real) {
        return Err("read_path_holds_home");
    }
    let secrets: Vec<PathBuf> = owner_secrets(&owner).collect();
    if clear_of_secrets(&real, &secrets).is_err() {
        return Err("read_path_credentials");
    }
    let data = canonical(data_root).unwrap_or_else(|_| data_root.to_path_buf());
    if real.starts_with(&data) || data.starts_with(&real) {
        return Err("read_path_engine_data");
    }
    granted_root(&real, &owner, &secrets).map_err(|_| "read_path_refused")?;
    Ok(real)
}

/// Checks a project's whole list of extra read-only folders as [`checked`] does, at most
/// [`EXTRA_PATHS_MAX`], dropping repeats. Inputs: the typed paths and the engine's data
/// folder. Output: the canonical folders in order, or the first refusal code.
pub fn checked_all(texts: &[String], data_root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if texts.len() > EXTRA_PATHS_MAX {
        return Err("read_paths_too_many");
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(texts.len());
    for text in texts {
        let path = checked(text, data_root)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    Ok(paths)
}

/// Replaces a project's extra read-only folders. Inputs: the open connection or
/// transaction, the project id and the checked folders. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, paths: &[PathBuf]) -> rusqlite::Result<()> {
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    connection.execute(TABLE, [])?;
    connection.execute("DELETE FROM project_read_paths WHERE project=?1", [project])?;
    for (position, path) in paths.iter().enumerate() {
        let text = path
            .to_str()
            .ok_or_else(|| rusqlite::Error::InvalidPath(path.clone()))?;
        connection.execute(
            "INSERT INTO project_read_paths (project, position, path) VALUES (?1, ?2, ?3)",
            params![project, position as i64, text],
        )?;
    }
    Ok(())
}

/// Reads a project's extra read-only folders in the order the owner saved them; none when
/// nothing was ever saved. Inputs: the open connection or transaction and the project id.
/// Output: the folders, or the database error.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<Vec<PathBuf>> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_read_paths')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT path FROM project_read_paths WHERE project=?1 ORDER BY position LIMIT ?2",
    )?;
    let paths = statement
        .query_map(params![project, EXTRA_PATHS_MAX as i64], |row| {
            row.get::<_, String>(0).map(PathBuf::from)
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    Ok(paths)
}
