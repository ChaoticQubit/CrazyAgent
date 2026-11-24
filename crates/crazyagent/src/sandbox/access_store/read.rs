use kernel_sandbox::extra_paths::EXTRA_PATHS_MAX;
use rusqlite::{Connection, params};
use std::path::PathBuf;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_read_paths (
    project TEXT NOT NULL,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project, position)
)";

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
/// nothing was ever saved. A folder saved under the data volume's long name comes back under
/// its short name, the one a sandbox rule matches. Inputs: the open connection or transaction
/// and the project id. Output: the folders, or the database error.
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
            row.get::<_, String>(0)
                .map(|text| kernel_sandbox::paths::one_spelling(PathBuf::from(text)))
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= EXTRA_PATHS_MAX);
    Ok(paths)
}

#[cfg(test)]
mod tests;
