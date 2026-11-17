use crate::engine::EngineError;
use crate::projects_home::reinstall::{self, KEPT_BYTES_MAX, Kept, OFFERS_MAX, Offer, bounded};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::{Path, PathBuf};

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
/// project's old folder; past `OFFERS_MAX` rows the oldest goes. A folder whose name is not
/// UTF-8 keeps no offer. Inputs: the connection, the landed folder, a project move's old
/// folder and the record. Output: Ok once committed, else `reinstall_offer_unavailable`.
pub(crate) fn keep(
    db: &mut Connection,
    landed: &Path,
    old: Option<&Path>,
    kept: &Kept,
) -> Result<(), EngineError> {
    assert!(bounded(kept));
    let body = serde_json::to_string(kept).map_err(|_| unavailable())?;
    assert!(body.len() <= KEPT_BYTES_MAX);
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| unavailable())?;
    tx.execute_batch(SCHEMA).map_err(|_| unavailable())?;
    if let Some(old) = old.and_then(Path::to_str) {
        delete(&tx, old)?;
    }
    match landed.to_str() {
        Some(folder) if kept.left.is_empty() => {
            delete(&tx, folder)?;
        }
        Some(folder) => {
            tx.execute(
                "INSERT OR REPLACE INTO projects_home_reinstall (folder, kept) VALUES (?1, ?2)",
                params![folder, body],
            )
            .map_err(|_| unavailable())?;
        }
        None => {}
    }
    tx.execute(
        "DELETE FROM projects_home_reinstall WHERE rowid <= (SELECT rowid FROM \
         projects_home_reinstall ORDER BY rowid DESC LIMIT 1 OFFSET ?1)",
        [OFFERS_MAX as i64],
    )
    .map_err(|_| unavailable())?;
    tx.commit().map_err(|_| unavailable())
}

/// Reads the record kept at a folder. Inputs: the connection and the folder. Output: the
/// record, None when there is none or the table is not made yet, or
/// `reinstall_offer_unreadable` for a stored record out of its bounds.
pub(crate) fn at(db: &Connection, folder: &Path) -> Result<Option<Kept>, EngineError> {
    let Some(folder) = folder.to_str() else {
        return Ok(None);
    };
    if !exists(db)? {
        return Ok(None);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT kept FROM projects_home_reinstall WHERE folder = ?1",
            [folder],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| unavailable())?;
    body.map(|body| read(&body)).transpose()
}

/// Reads a project's offer from the record at its folder. Inputs: the connection and the
/// project. Output: the offer, None for no record or an unknown project.
pub(crate) fn offer(db: &Connection, project: &ProjectId) -> Result<Option<Offer>, EngineError> {
    let Some(folder) = folder_of(db, project)? else {
        return Ok(None);
    };
    Ok(at(db, &folder)?.map(|kept| reinstall::offer(&kept)))
}

/// Dismisses a project's offer: deletes the record at its folder. Inputs: the connection
/// and the project. Output: whether a record was deleted.
pub(crate) fn dismiss(db: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    let Some(folder) = folder_of(db, project)? else {
        return Ok(false);
    };
    let Some(folder) = folder.to_str() else {
        return Ok(false);
    };
    if !exists(db)? {
        return Ok(false);
    }
    let deleted = db
        .execute(
            "DELETE FROM projects_home_reinstall WHERE folder = ?1",
            [folder],
        )
        .map_err(|_| unavailable())?;
    assert!(deleted <= 1);
    Ok(deleted == 1)
}

/// Deletes the record at a removed project's folder, inside the removal's transaction.
/// Inputs: the transaction and the folder. Output: Ok, else `reinstall_offer_unavailable`.
pub(crate) fn forget(tx: &Transaction<'_>, folder: &Path) -> Result<(), EngineError> {
    match folder.to_str() {
        Some(folder) if exists(tx)? => delete(tx, folder),
        _ => Ok(()),
    }
}

/// The folder of a set-up project, where its offer is kept. Inputs: the connection and the
/// project. Output: the folder, or None for a project with no setup.
fn folder_of(db: &Connection, project: &ProjectId) -> Result<Option<PathBuf>, EngineError> {
    Ok(crate::project_setup::load_setup(db, project)?.map(|stored| stored.project_folder()))
}

/// Deletes the row at a folder. Inputs: the transaction and the folder. Output: Ok.
fn delete(tx: &Transaction<'_>, folder: &str) -> Result<(), EngineError> {
    tx.execute(
        "DELETE FROM projects_home_reinstall WHERE folder = ?1",
        [folder],
    )
    .map(|_| ())
    .map_err(|_| unavailable())
}

/// Reads a stored record, refusing one that is not a record or is out of its bounds.
/// Input: the stored JSON. Output: the record, else `reinstall_offer_unreadable`.
fn read(body: &str) -> Result<Kept, EngineError> {
    let unreadable = || EngineError("reinstall_offer_unreadable".into());
    let kept: Kept = serde_json::from_str(body).map_err(|_| unreadable())?;
    match bounded(&kept) {
        true => Ok(kept),
        false => Err(unreadable()),
    }
}

/// Whether the offers table is made yet. Input: the connection. Output: the answer.
fn exists(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
         'projects_home_reinstall')",
        [],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|_| unavailable())
}

/// The code for a database failure while keeping or reading an offer.
fn unavailable() -> EngineError {
    EngineError("reinstall_offer_unavailable".into())
}
