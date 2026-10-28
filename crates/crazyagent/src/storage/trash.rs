use super::error;
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use std::path::Path;

pub(crate) const ACCOUNT_KEY: &str = "account_key";
pub(crate) const MCP_SECRET: &str = "mcp_secret";
pub(crate) const MCP_CHECKS: &str = "mcp_checks";
const SWEEP_MAX: i64 = 10_000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS file_trash (
    kind TEXT NOT NULL CHECK(kind IN ('account_key','mcp_secret','mcp_checks')),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    PRIMARY KEY(kind, name)
) WITHOUT ROWID;";

/// Creates the table of secret files a committed removal left to delete.
/// Input: the engine connection. Output: any SQLite failure.
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

/// Deletes every file the trash names under the engine root, at most `SWEEP_MAX` a start,
/// and forgets each one that is gone. A name a live record still refers to is forgotten
/// without deleting it. A file that cannot be deleted stays noted for the next start.
/// Inputs: the engine connection and root. Output: how many notes are left.
pub(crate) fn sweep(connection: &Connection, root: &Path) -> Result<usize, EngineError> {
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
        let gone = if referenced(connection, &kind, &name)? {
            Ok(())
        } else {
            delete(root, &kind, &name)
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
    }
    let left: i64 = connection
        .query_row("SELECT count(*) FROM file_trash", [], |row| row.get(0))
        .map_err(error)?;
    usize::try_from(left).map_err(error)
}

fn referenced(connection: &Connection, kind: &str, name: &str) -> Result<bool, EngineError> {
    let quoted = format!("\"{name}\"");
    let (query, value) = match kind {
        ACCOUNT_KEY => (
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
        _ => return Err(error("file_trash_kind_unknown")),
    };
    connection
        .query_row(query, [value], |row| row.get(0))
        .map_err(error)
}

fn delete(root: &Path, kind: &str, name: &str) -> Result<(), EngineError> {
    match kind {
        ACCOUNT_KEY => super::accounts::remove_key_file(root, name),
        MCP_SECRET => super::plugins::remove_secret(root, name),
        MCP_CHECKS => super::plugins::remove_check_folder(root, name),
        _ => Err(error("file_trash_kind_unknown")),
    }
}

#[cfg(all(test, unix))]
#[path = "trash_tests.rs"]
mod tests;
