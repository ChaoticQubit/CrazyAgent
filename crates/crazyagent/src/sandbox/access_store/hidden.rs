use kernel_sandbox::hidden_paths::HIDDEN_PATHS_MAX;
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
use std::path::PathBuf;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS project_hidden_paths (
    project TEXT NOT NULL,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project, position)
)";

const SEERS: &str = "team_hidden_seers";

const SEERS_TABLE: &str = "CREATE TABLE IF NOT EXISTS team_hidden_seers (
    agent INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    restart INTEGER NOT NULL DEFAULT 0 CHECK(restart IN (0, 1))
)";

/// Replaces the running agents of one project that can still open a folder hidden after
/// they started, writing only when the set changed; an agent kept on the list keeps the
/// owner's restart ask. Inputs: the engine connection, the project id and the agents.
/// Output: done, or the database error.
pub fn set_seers(
    connection: &Connection,
    project: &str,
    agents: &BTreeSet<i64>,
) -> rusqlite::Result<()> {
    let listed = seers(connection, project)?;
    if &listed == agents {
        return Ok(());
    }
    let tx = connection.unchecked_transaction()?;
    tx.execute(SEERS_TABLE, [])?;
    for gone in listed.difference(agents) {
        tx.execute(
            "DELETE FROM team_hidden_seers WHERE agent=?1 AND project=?2",
            params![gone, project],
        )?;
    }
    for agent in agents.difference(&listed) {
        tx.execute(
            "INSERT OR REPLACE INTO team_hidden_seers(agent, project, restart) VALUES (?1, ?2, 0)",
            params![agent, project],
        )?;
    }
    tx.commit()
}

/// Drops one project's list of agents that can still open a hidden folder, as removing
/// the project does. Inputs: the open transaction and the project id. Output: done, or the
/// database error.
pub fn forget_seers(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    if present(connection)? {
        connection.execute("DELETE FROM team_hidden_seers WHERE project=?1", [project])?;
    }
    Ok(())
}

/// Tells whether the engine ever kept a list of agents that can still open a hidden
/// folder. Input: the connection. Output: the answer, or the database error.
fn present(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [SEERS],
        |row| row.get(0),
    )
}

/// Reads one project's listed agents, only those the owner asked to restart when `asked`
/// holds. Inputs: the connection, the project id and the choice. Output: their ids, none
/// without a list, or the database error.
fn listed(connection: &Connection, project: &str, asked: bool) -> rusqlite::Result<BTreeSet<i64>> {
    if !present(connection)? {
        return Ok(BTreeSet::new());
    }
    let mut statement = connection.prepare(if asked {
        "SELECT agent FROM team_hidden_seers WHERE project=?1 AND restart=1"
    } else {
        "SELECT agent FROM team_hidden_seers WHERE project=?1"
    })?;
    statement.query_map([project], |row| row.get(0))?.collect()
}

/// Reads the agents of one project listed as able to open a folder hidden after they
/// started; a database without the list has none. Inputs: the connection and the project
/// id. Output: their ids.
pub fn seers(connection: &Connection, project: &str) -> rusqlite::Result<BTreeSet<i64>> {
    listed(connection, project, false)
}

/// Asks every listed agent of one project to restart, as the owner's Restart them does.
/// Inputs: the open transaction and the project id. Output: how many listed agents were
/// asked, none for a project with none listed, or the database error.
pub fn ask_restart(connection: &Connection, project: &str) -> rusqlite::Result<usize> {
    if !present(connection)? {
        return Ok(0);
    }
    connection.execute(
        "UPDATE team_hidden_seers SET restart=1 WHERE project=?1",
        [project],
    )
}

/// Reads the listed agents of one project the owner asked to restart. Inputs: the
/// connection and the project id. Output: their ids.
pub fn restarts(connection: &Connection, project: &str) -> rusqlite::Result<BTreeSet<i64>> {
    listed(connection, project, true)
}

/// Replaces a project's hidden folders. Inputs: the open connection or transaction, the
/// project id and the checked folders. Output: done, or the database error.
pub fn save(connection: &Connection, project: &str, paths: &[PathBuf]) -> rusqlite::Result<()> {
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    connection.execute(TABLE, [])?;
    connection.execute(
        "DELETE FROM project_hidden_paths WHERE project=?1",
        [project],
    )?;
    for (position, path) in paths.iter().enumerate() {
        let text = path
            .to_str()
            .ok_or_else(|| rusqlite::Error::InvalidPath(path.clone()))?;
        connection.execute(
            "INSERT INTO project_hidden_paths (project, position, path) VALUES (?1, ?2, ?3)",
            params![project, position as i64, text],
        )?;
    }
    Ok(())
}

/// Reads a project's hidden folders in the order the owner saved them; none when nothing was
/// ever saved. A folder saved under the data volume's long name comes back under its short
/// name, the one a sandbox root has. Inputs: the open connection or transaction and the
/// project id. Output: the folders, or the database error.
pub fn load(connection: &Connection, project: &str) -> rusqlite::Result<Vec<PathBuf>> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_hidden_paths')",
        [],
        |row| row.get(0),
    )?;
    if !present {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT path FROM project_hidden_paths WHERE project=?1 ORDER BY position LIMIT ?2",
    )?;
    let paths = statement
        .query_map(params![project, HIDDEN_PATHS_MAX as i64], |row| {
            row.get::<_, String>(0)
                .map(|text| kernel_sandbox::paths::one_spelling(PathBuf::from(text)))
        })?
        .collect::<rusqlite::Result<Vec<PathBuf>>>()?;
    assert!(paths.len() <= HIDDEN_PATHS_MAX);
    Ok(paths)
}

#[cfg(test)]
mod tests;
