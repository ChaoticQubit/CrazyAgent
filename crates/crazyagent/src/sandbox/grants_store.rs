use kernel_sandbox::grants::LogView;
use kernel_sandbox::policy::AccessGrant;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

/// The most log rows one project keeps; older ones are pruned on each write.
pub const LOG_KEPT_MAX: i64 = 256;

/// The most log rows a project's Agent access view lists, newest first.
pub const LOG_LISTED_MAX: usize = 20;

const LOG_TABLE: &str = "CREATE TABLE IF NOT EXISTS access_grant_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('granted','revoked','allowed','denied')),
    grant_json TEXT NOT NULL,
    by TEXT NOT NULL
)";

const REQUESTS_TABLE: &str = "CREATE TABLE IF NOT EXISTS access_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    asker TEXT NOT NULL,
    grant_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    question INTEGER,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','allowed','denied')),
    created_ms INTEGER NOT NULL,
    identity TEXT
)";

const GRANT_JSON_BYTES_MAX: usize = 256;

const _: () = assert!(kernel_sandbox::policy::GRANT_SOCKET_BYTES_MAX + 64 <= GRANT_JSON_BYTES_MAX);

/// The engine's data folder for a connection on `engine.db`: the folder holding it.
/// Input: the connection. Output: the folder, or none for an in-memory database.
pub fn data_root(connection: &Connection) -> Option<PathBuf> {
    connection
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Logs one grant change for a project and prunes the log to [`LOG_KEPT_MAX`] rows.
/// Inputs: the connection or transaction, the project, the action (`granted`, `revoked`,
/// `allowed` or `denied`), the grant, who made it and the time in ms. Output: done, or the
/// database error.
pub fn log(
    connection: &Connection,
    project: &str,
    action: &str,
    grant: &AccessGrant,
    by: &str,
    now: i64,
) -> rusqlite::Result<()> {
    assert!(matches!(
        action,
        "granted" | "revoked" | "allowed" | "denied"
    ));
    let body = serde_json::to_string(grant)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    assert!(body.len() <= GRANT_JSON_BYTES_MAX);
    connection.execute(LOG_TABLE, [])?;
    connection.execute(
        "INSERT INTO access_grant_log(project,at_ms,action,grant_json,by) VALUES (?1,?2,?3,?4,?5)",
        params![project, now, action, body, by],
    )?;
    connection.execute(
        "DELETE FROM access_grant_log WHERE project=?1 AND id NOT IN (
             SELECT id FROM access_grant_log WHERE project=?1 ORDER BY id DESC LIMIT ?2)",
        params![project, LOG_KEPT_MAX],
    )?;
    Ok(())
}

/// Logs what an owner save changed in a project's grants: each grant only in the new list
/// as granted and each only in the old one as revoked. Inputs: the connection, the project,
/// the grants before and after, who saved and the time. Output: done, or the database error.
pub fn log_changes(
    connection: &Connection,
    project: &str,
    before: &[AccessGrant],
    after: &[AccessGrant],
    by: &str,
    now: i64,
) -> rusqlite::Result<()> {
    for grant in after.iter().filter(|grant| !before.contains(grant)) {
        log(connection, project, "granted", grant, by, now)?;
    }
    for grant in before.iter().filter(|grant| !after.contains(grant)) {
        log(connection, project, "revoked", grant, by, now)?;
    }
    Ok(())
}

/// Reads a project's newest grant log rows, at most [`LOG_LISTED_MAX`]; none when nothing
/// was ever logged. Inputs: the connection and the project. Output: the rows, newest first.
pub fn history(connection: &Connection, project: &str) -> rusqlite::Result<Vec<LogView>> {
    if !table_exists(connection, "access_grant_log")? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT at_ms,action,grant_json,by FROM access_grant_log WHERE project=?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![project, LOG_LISTED_MAX as i64], |row| {
            let body: String = row.get(2)?;
            let grant = serde_json::from_str::<AccessGrant>(&body)
                .map(|grant| grant.label())
                .unwrap_or_else(|_| "An extra this version does not know".to_owned());
            Ok(LogView {
                at_ms: row.get(0)?,
                action: row.get(1)?,
                grant,
                by: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(rows.len() <= LOG_LISTED_MAX);
    Ok(rows)
}

/// Removes a project's grant log and requests when the project is removed. Inputs: the
/// transaction and the project. Output: done, or the database error.
pub fn forget_project(connection: &Connection, project: &str) -> rusqlite::Result<()> {
    connection.execute(LOG_TABLE, [])?;
    requests_table(connection)?;
    connection.execute("DELETE FROM access_grant_log WHERE project=?1", [project])?;
    connection.execute("DELETE FROM access_requests WHERE project=?1", [project])?;
    Ok(())
}

/// Makes the access requests table, and adds the `identity` column to one an older engine
/// made, whose rows keep none. Input: the connection. Output: done, or the store error.
pub(crate) fn requests_table(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(REQUESTS_TABLE, [])?;
    let kept: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('access_requests') WHERE name='identity')",
        [],
        |row| row.get(0),
    )?;
    if !kept {
        connection.execute("ALTER TABLE access_requests ADD COLUMN identity TEXT", [])?;
    }
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
}
