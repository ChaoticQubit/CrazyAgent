use crate::StorageError;
use crate::migrations::error;
use rusqlite::{Transaction, params};

/// The most event bodies [`logged`] reads at once.
pub const CATCH_UP_MAX: usize = 128;

#[cfg(test)]
mod tests;

/// The bodies of the events after `after`, at most [`CATCH_UP_MAX`] in cursor order, or none
/// for a cursor older than the retained events or ahead of the store. Inputs: the
/// transaction, the cursor and the store's revision. Output: the bodies, or the store's error.
pub fn logged(
    tx: &Transaction<'_>,
    after: u64,
    revision: u64,
) -> Result<Option<Vec<String>>, StorageError> {
    let first: Option<i64> = tx
        .query_row("SELECT MIN(cursor) FROM events", [], |row| row.get(0))
        .map_err(error)?;
    if after > revision || first.is_some_and(|first| after.saturating_add(1) < first as u64) {
        return Ok(None);
    }
    let mut statement = tx
        .prepare("SELECT body FROM events WHERE cursor>?1 ORDER BY cursor LIMIT ?2")
        .map_err(error)?;
    let rows = statement
        .query_map(params![after as i64, CATCH_UP_MAX as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?;
    let bodies = rows
        .map(|row| row.map_err(error))
        .collect::<Result<Vec<String>, _>>()?;
    assert!(bodies.len() <= CATCH_UP_MAX);
    Ok(Some(bodies))
}

/// Moves the store's revision to `revision`, logs `body` under it and prunes the events at
/// or below `revision - retained`. Inputs: the transaction, the revision, the event's body
/// and how many events the store keeps. Output: Ok, or the store's error.
pub fn persist(
    tx: &Transaction<'_>,
    revision: u64,
    body: &str,
    retained: u64,
) -> Result<(), StorageError> {
    assert!(revision > 0);
    tx.execute("UPDATE state SET revision=?1 WHERE id=1", [revision as i64])
        .map_err(error)?;
    tx.execute(
        "INSERT INTO events VALUES (?1,?2)",
        params![revision as i64, body],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM events WHERE cursor<=?1",
        [revision.saturating_sub(retained) as i64],
    )
    .map_err(error)?;
    Ok(())
}
