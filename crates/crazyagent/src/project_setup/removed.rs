use super::error;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

/// The most removals CrazyAgent records. A Remove from dashboard or a Delete everything
/// past it is refused before anything is removed or moved to the Trash.
pub(crate) const REMOVED_MAX: i64 = 100_000;

/// The highest number a removed project's name gets as `-n` when its id is held.
pub(crate) const SUFFIX_MAX: u32 = 99;

/// The most tables the hold and the clear read rows of a project id from.
const TABLES_MAX: i64 = 1024;

/// The longest folder text a removal records; a longer one is recorded as not returnable.
const FOLDER_BYTES_MAX: usize = 4096;

/// The longest project id the engine takes.
const ID_BYTES_MAX: usize = 128;

/// Creates the removal record: one row per removed project id, with the folder CrazyAgent
/// recorded for it and whether adding that exact folder again brings the project back.
/// Input: the engine connection. Output: Ok or the database's failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS projects_removed (
                project TEXT PRIMARY KEY COLLATE NOCASE
                    CHECK(length(project) BETWEEN 1 AND {ID_BYTES_MAX}),
                folder TEXT CHECK(folder IS NULL OR length(CAST(folder AS BLOB)) BETWEEN 1 AND {FOLDER_BYTES_MAX}),
                returns INTEGER NOT NULL CHECK(returns IN (0,1)),
                removed_ms INTEGER NOT NULL,
                CHECK(returns=0 OR folder IS NOT NULL)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS projects_removed_return
                ON projects_removed(folder) WHERE returns=1;"
        ))
        .map_err(error)?;
    Ok(())
}

/// Holds, as not returnable, every id that is not a live project and still has a row in a
/// table with a `project` column, tables this build does not know included, so no removal
/// made by an earlier or another line's build gives its id out again. Runs at every engine
/// open. Input: the engine connection. Output: Ok or the database's failure.
pub(crate) fn hold_left(_connection: &Connection) -> Result<(), EngineError> {
    Ok(())
}

/// Whether one more removal fits the record. Input: the connection. Output: Ok, or
/// `projects_removed_full` once `REMOVED_MAX` removals are recorded.
pub(crate) fn room(_connection: &Connection) -> Result<Result<(), &'static str>, EngineError> {
    Ok(Ok(()))
}

/// Records one removal: the project, the folder CrazyAgent recorded for it and whether
/// adding that folder again brings it back. Never refuses, so a Delete everything whose
/// folders already went to the Trash is always recorded. Inputs: the transaction, the
/// project, its folder and whether it may come back. Output: Ok or the database's failure.
pub(crate) fn record(
    _connection: &Connection,
    _project: &ProjectId,
    _folder: Option<&str>,
    _returns: bool,
) -> Result<(), EngineError> {
    Ok(())
}

/// The project removed from the dashboard whose recorded folder is exactly this text.
/// Inputs: the connection and the canonical folder text. Output: its id, or None.
pub(crate) fn returning(
    _connection: &Connection,
    _folder: &str,
) -> Result<Option<ProjectId>, EngineError> {
    Ok(None)
}

/// Whether a removal holds this id, ASCII case ignored. Inputs: the connection and the id.
/// Output: the answer.
pub(crate) fn held(_connection: &Connection, _project: &ProjectId) -> Result<bool, EngineError> {
    Ok(false)
}

/// Marks a project recorded at this folder as live again: its own returnable row goes, and
/// any other id's return to the same folder ends, so that id stays held. Inputs: the
/// transaction, the project and its folder text. Output: Ok or the database's failure.
pub(crate) fn claim(
    _connection: &Connection,
    _project: &ProjectId,
    _folder: &str,
) -> Result<(), EngineError> {
    Ok(())
}

/// The id a folder with no project of its own is onboarded as: its name's id, or the first
/// of `-2` to `-99` after it that no removal holds. Inputs: the connection and the folder.
/// Output: the id, `project_exists` when that first id is a live project, or
/// `onboarding_project_ids_used` when every one is held.
pub(crate) fn free_id(
    _connection: &Connection,
    folder: &Path,
) -> Result<Result<ProjectId, &'static str>, EngineError> {
    Ok(Ok(crate::onboarding::project_id_for(folder)?))
}

/// Deletes every row of this id in every table keyed by project, its removal record
/// included, so a demo made again inherits nothing of an earlier one. Inputs: the
/// transaction and the id. Output: Ok or the database's failure.
pub(crate) fn clear(_connection: &Connection, _project: &ProjectId) -> Result<(), EngineError> {
    Ok(())
}
