use crate::engine::EngineError;
use crate::projects_home::reinstall::{KEPT_BYTES_MAX, Kept, Offer};
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction};
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

/// One reinstall offer per folder a job landed in, kept until the owner dismisses it, the
/// project is removed, or a later job lands there again (DEC-198 slice 3).
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS projects_home_reinstall (
    folder TEXT PRIMARY KEY CHECK (length(CAST(folder AS BLOB)) BETWEEN 1 AND 4096),
    kept TEXT NOT NULL CHECK (length(CAST(kept AS BLOB)) <= 65536)
);
";
const _: () = assert!(KEPT_BYTES_MAX <= 65536);

/// Keeps the offer of a job that ended done: replaces the row at the folder it landed in,
/// or deletes it when the job left nothing to reinstall, and deletes the row at a moved
/// project's old folder; past `OFFERS_MAX` rows the oldest goes. Inputs: the connection,
/// the landed folder, a project move's old folder and the record. Output: Ok once committed.
pub(crate) fn keep(
    db: &mut Connection,
    landed: &Path,
    old: Option<&Path>,
    kept: &Kept,
) -> Result<(), EngineError> {
    let _ = (db, landed, old, kept, SCHEMA);
    Ok(())
}

/// Reads the record kept at a folder. Inputs: the connection and the folder. Output: the
/// record, None when there is none or the table is not made yet, or
/// `reinstall_offer_unreadable` for a stored record out of its bounds.
pub(crate) fn at(db: &Connection, folder: &Path) -> Result<Option<Kept>, EngineError> {
    let _ = (db, folder);
    Ok(None)
}

/// Reads a project's offer from the record at its folder. Inputs: the connection and the
/// project. Output: the offer, None for no record or an unknown project.
pub(crate) fn offer(db: &Connection, project: &ProjectId) -> Result<Option<Offer>, EngineError> {
    let _ = (db, project);
    Ok(None)
}

/// Dismisses a project's offer: deletes the record at its folder. Inputs: the connection
/// and the project. Output: whether a record was deleted.
pub(crate) fn dismiss(db: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    let _ = (db, project);
    Ok(false)
}

/// Deletes the record at a removed project's folder, inside the removal's transaction.
/// Inputs: the transaction and the folder. Output: Ok.
pub(crate) fn forget(tx: &Transaction<'_>, folder: &Path) -> Result<(), EngineError> {
    let _ = (tx, folder);
    Ok(())
}
