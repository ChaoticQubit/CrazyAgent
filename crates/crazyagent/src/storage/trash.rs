use super::error;
use crate::config::ModelAccess;
#[cfg(unix)]
use crate::config::ModelAuthentication;
use crate::engine::EngineError;
use rusqlite::{Connection, params};
#[cfg(unix)]
use std::path::Path;

pub(crate) const ACCOUNT_KEY: &str = "account_key";
pub(crate) const MCP_SECRET: &str = "mcp_secret";
pub(crate) const MCP_CHECKS: &str = "mcp_checks";
pub(crate) const SKILL_FOLDER: &str = "skill_folder";
pub(crate) const HOOK_FOLDER: &str = "hook_folder";
#[cfg(unix)]
pub(crate) const NATIVE_SIGN_IN: &str = "native_sign_in";
#[cfg(unix)]
pub(crate) const GENERATED_HOME: &str = "generated_home";
#[cfg(unix)]
const SWEEP_MAX: i64 = 10_000;

#[cfg(unix)]
macro_rules! trash_columns {
    () => {
        "(
    kind TEXT NOT NULL CHECK(kind IN ('account_key','mcp_secret','mcp_checks','skill_folder','hook_folder','native_sign_in','generated_home')),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    PRIMARY KEY(kind, name)
) WITHOUT ROWID"
    };
}

#[cfg(unix)]
const SCHEMA: &str = concat!(
    "CREATE TABLE IF NOT EXISTS file_trash ",
    trash_columns!(),
    ";"
);
/// Rebuilds a trash table from before hook folders with the current check, keeping every
/// note, for the upgrade step that lets it hold hook folders.
#[cfg(unix)]
pub(super) const WIDEN: &str = concat!(
    "CREATE TABLE file_trash_next ",
    trash_columns!(),
    ";
INSERT INTO file_trash_next(kind,name) SELECT kind,name FROM file_trash;
DROP TABLE file_trash;
ALTER TABLE file_trash_next RENAME TO file_trash;"
);

/// Creates the table of files a committed removal left to delete.
/// Input: the engine connection. Output: any SQLite failure.
#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(error)
}

/// Records, inside the removal's own transaction, a file the removal leaves to delete, so
/// a crash before the file is gone still leaves a durable note for the next start.
/// Inputs: the transaction, the kind and the file's name. Output: any SQLite failure.
pub(crate) fn put(tx: &Connection, kind: &str, name: &str) -> Result<(), EngineError> {
    tx.execute(
        "INSERT OR IGNORE INTO file_trash(kind,name) VALUES (?1,?2)",
        params![kind, name],
    )
    .map_err(error)?;
    Ok(())
}

/// Deletes what the removals left noted when the engine starts, after a settings write a
/// crash left pending is recovered, so the catalog it checks is the one the engine runs on.
/// Inputs: the engine connection and the catalog. Output: any failure.
#[cfg(unix)]
pub(crate) fn sweep_at_start(
    connection: &Connection,
    catalog: Option<&ModelAccess>,
) -> Result<(), EngineError> {
    let root = connection
        .path()
        .and_then(|path| Path::new(path).parent())
        .ok_or_else(|| error("file_trash_root_unknown"))?;
    sweep(connection, root, catalog).map(|_| ())
}

/// Leaves the trash alone where engine storage does not open.
/// Inputs: the engine connection and the catalog, both unused. Output: always done.
#[cfg(not(unix))]
pub(crate) fn sweep_at_start(
    _connection: &Connection,
    _catalog: Option<&ModelAccess>,
) -> Result<(), EngineError> {
    Ok(())
}

/// Deletes every file the trash names under the engine root, at most `SWEEP_MAX` a start,
/// and forgets each one that is gone. A name a live record or a catalog connection still
/// refers to is forgotten without deleting it. A file that cannot be deleted stays noted
/// for the next start. Inputs: the engine connection, root and catalog. Output: how many
/// notes are left.
#[cfg(unix)]
pub(crate) fn sweep(
    connection: &Connection,
    root: &Path,
    catalog: Option<&ModelAccess>,
) -> Result<usize, EngineError> {
    let notes = connection
        .prepare("SELECT kind,name FROM file_trash ORDER BY kind,name LIMIT ?1")
        .map_err(error)?
        .query_map([SWEEP_MAX], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    for (kind, name) in notes {
        settle(connection, root, catalog, &kind, &name)?;
    }
    let left: i64 = connection
        .query_row("SELECT count(*) FROM file_trash", [], |row| row.get(0))
        .map_err(error)?;
    usize::try_from(left).map_err(error)
}

/// Deletes one noted file unless a live record or a catalog connection still names it, and
/// forgets the note once the file is gone; a file that cannot be deleted stays noted for the
/// next sweep. Inputs: the engine connection, root and catalog, the kind and the name.
/// Output: any SQLite failure.
#[cfg(unix)]
pub(crate) fn settle(
    connection: &Connection,
    root: &Path,
    catalog: Option<&ModelAccess>,
    kind: &str,
    name: &str,
) -> Result<(), EngineError> {
    let gone = if referenced(connection, catalog, kind, name)? {
        Ok(())
    } else {
        delete(root, kind, name)
    };
    match gone {
        Ok(()) => {
            connection
                .execute(
                    "DELETE FROM file_trash WHERE kind=?1 AND name=?2",
                    params![kind, name],
                )
                .map_err(error)?;
        }
        Err(problem) => eprintln!("file_trash_not_removed kind={kind} cause={}", problem.0),
    }
    Ok(())
}

/// Tells whether a connection left in the catalog still names a reference, the one name of
/// an account's key, generated home or native profile. Inputs: the catalog and the name.
/// Output: true when one does.
#[cfg(unix)]
fn in_catalog(catalog: Option<&ModelAccess>, name: &str) -> bool {
    catalog.is_some_and(|access| {
        access
            .connections
            .iter()
            .any(|connection| match &connection.auth {
                ModelAuthentication::ApiKey { credential_ref }
                | ModelAuthentication::Oauth { credential_ref } => credential_ref.as_str() == name,
                ModelAuthentication::NativeManaged { profile_ref } => profile_ref.as_str() == name,
                ModelAuthentication::None {} => false,
            })
    })
}

#[cfg(unix)]
fn referenced(
    connection: &Connection,
    catalog: Option<&ModelAccess>,
    kind: &str,
    name: &str,
) -> Result<bool, EngineError> {
    if matches!(kind, ACCOUNT_KEY | NATIVE_SIGN_IN | GENERATED_HOME) && in_catalog(catalog, name) {
        return Ok(true);
    }
    let quoted = format!("\"{name}\"");
    let scoped = format!("project:{name}");
    let stored = format!("/{name}\"");
    let (query, value) = match kind {
        ACCOUNT_KEY | NATIVE_SIGN_IN | GENERATED_HOME => (
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE instr(CAST(body AS TEXT), ?1) > 0)",
            quoted.as_str(),
        ),
        MCP_SECRET => (
            "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE instr(body, ?1) > 0)",
            quoted.as_str(),
        ),
        MCP_CHECKS => (
            "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE id=?1)",
            name,
        ),
        SKILL_FOLDER => (
            "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE kind='skill' AND lower(scope)=lower(?1))",
            scoped.as_str(),
        ),
        HOOK_FOLDER => (
            "SELECT EXISTS(SELECT 1 FROM plugin_items WHERE kind='hook' AND instr(body, ?1) > 0)",
            stored.as_str(),
        ),
        _ => return Err(error("file_trash_kind_unknown")),
    };
    connection
        .query_row(query, [value], |row| row.get(0))
        .map_err(error)
}

/// Reads a note's name as a credential reference. Input: the name. Output: the reference,
/// or `account_key_reference_invalid`.
#[cfg(unix)]
fn reference(name: &str) -> Result<crate::protocol::CredentialRef, EngineError> {
    crate::protocol::CredentialRef::new(name).map_err(|_| error("account_key_reference_invalid"))
}

#[cfg(unix)]
fn delete(root: &Path, kind: &str, name: &str) -> Result<(), EngineError> {
    match kind {
        ACCOUNT_KEY => super::accounts::remove_key_file(root, name),
        MCP_SECRET => super::plugins::remove_secret(root, name),
        MCP_CHECKS => super::plugins::remove_check_folder(root, name),
        SKILL_FOLDER => super::plugins::remove_skill_project_folder(root, name),
        HOOK_FOLDER => super::plugins::remove_hook_folder(root, name),
        NATIVE_SIGN_IN => crate::engine::bootstrap::remove_sign_in(root, &reference(name)?),
        GENERATED_HOME => crate::engine::bootstrap::remove_generated_home(root, &reference(name)?),
        _ => Err(error("file_trash_kind_unknown")),
    }
}

#[cfg(all(test, unix))]
#[path = "trash_tests.rs"]
mod tests;
