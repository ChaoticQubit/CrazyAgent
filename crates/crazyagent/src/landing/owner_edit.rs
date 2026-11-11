use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// The most owner-edited paths one agent worktree keeps waiting for its next turn end.
pub const PATHS_PER_WORKTREE_MAX: i64 = 1000;
/// The most bytes an owner-edited path may hold.
pub const PATH_BYTES_MAX: usize = 4096;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS landing_owner_edits (
    worktree TEXT NOT NULL,
    path TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    change_set TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (worktree, change_set, path)
);";

/// Records that the owner edited `path` in an agent's worktree at `at`. The edit waits
/// for the worktree's next turn end, which settles it on the change set that turn end
/// records, so only that change set marks the file `OwnerEdited`. Inputs: the store, the
/// agent's worktree, the path relative to it and the time in ms. Output: Ok once kept,
/// `changes_path_invalid` for a path outside the worktree or `changes_owner_edits_full`
/// past `PATHS_PER_WORKTREE_MAX` waiting paths.
pub fn record(db: &Connection, worktree: &Path, path: &str, at: u64) -> Result<(), EngineError> {
    check_path(path)?;
    let worktree = worktree
        .to_str()
        .filter(|_| worktree.is_absolute())
        .ok_or_else(invalid)?;
    let at = i64::try_from(at).map_err(|_| invalid())?;
    db.execute_batch(SCHEMA).map_err(storage)?;
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)
        .map_err(storage)?;
    let known: Option<i64> = tx
        .query_row(
            "SELECT at_ms FROM landing_owner_edits
             WHERE worktree=?1 AND change_set='' AND path=?2",
            params![worktree, path],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    if known.is_none() {
        let kept: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM landing_owner_edits WHERE worktree=?1 AND change_set=''",
                [worktree],
                |row| row.get(0),
            )
            .map_err(storage)?;
        assert!(kept <= PATHS_PER_WORKTREE_MAX);
        if kept == PATHS_PER_WORKTREE_MAX {
            return Err(EngineError("changes_owner_edits_full".into()));
        }
    }
    tx.execute(
        "INSERT INTO landing_owner_edits(worktree, path, at_ms) VALUES (?1,?2,?3)
         ON CONFLICT(worktree, change_set, path) DO UPDATE SET at_ms=excluded.at_ms",
        params![worktree, path, at],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// Checks that a path names a file inside a worktree the way git names it: relative,
/// `/`-separated names with no empty, `.` or `..` part, at most `PATH_BYTES_MAX` bytes, no
/// NUL. Input: the path. Output: Ok, or `changes_path_invalid`.
pub fn check_path(path: &str) -> Result<(), EngineError> {
    let plain = path.len() <= PATH_BYTES_MAX
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !plain {
        return Err(invalid());
    }
    Ok(())
}

/// Settles the owner edits waiting in a worktree on the change set its turn end just
/// recorded, or drops them when the turn end left none open because nothing differs.
/// Failing leaves them waiting for the next turn end. Inputs: the store, the worktree and
/// the change set the turn end returned. Output: Ok once settled.
#[cfg(unix)]
pub(super) fn settle(
    db: &Connection,
    worktree: &Path,
    kept: Option<&super::ChangeSetId>,
) -> Result<(), EngineError> {
    let worktree = worktree.to_str().ok_or_else(invalid)?;
    db.execute_batch(SCHEMA).map_err(storage)?;
    match kept {
        Some(id) => db.execute(
            "UPDATE OR REPLACE landing_owner_edits SET change_set=?2
             WHERE worktree=?1 AND change_set=''",
            params![worktree, id.as_str()],
        ),
        None => db.execute(
            "DELETE FROM landing_owner_edits WHERE worktree=?1 AND change_set=''",
            [worktree],
        ),
    }
    .map_err(storage)?;
    Ok(())
}

/// Forgets the owner edits settled on a change set once no review can show them again:
/// it was accepted, or discarded and pruned past its restore window. Inputs: the store and
/// the change set. Output: Ok once forgotten.
#[cfg(unix)]
pub(super) fn forget(db: &Connection, change_set: &super::ChangeSetId) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)?;
    db.execute(
        "DELETE FROM landing_owner_edits WHERE change_set=?1",
        [change_set.as_str()],
    )
    .map_err(storage)?;
    Ok(())
}

/// Forgets every owner edit of a worktree, waiting or settled, once the worktree is
/// removed. Inputs: the store and the worktree. Output: Ok once forgotten.
#[cfg(unix)]
pub(super) fn forget_worktree(db: &Connection, worktree: &Path) -> Result<(), EngineError> {
    let worktree = worktree.to_str().ok_or_else(invalid)?;
    db.execute_batch(SCHEMA).map_err(storage)?;
    db.execute(
        "DELETE FROM landing_owner_edits WHERE worktree=?1",
        [worktree],
    )
    .map_err(storage)?;
    Ok(())
}

/// Logs a failed cleanup of owner edits by its plain code. The step it followed already
/// took effect, so its outcome stands and only the rows wait for a later cleanup. Inputs:
/// the step and the failure. Output: none.
#[cfg(unix)]
pub(super) fn logged(step: &str, failed: Result<(), EngineError>) {
    if let Err(problem) = failed {
        eprintln!(
            "landing_owner_edits_{step}_failed cause={}",
            crate::protocol::plain_code(&problem.0)
        );
    }
}

/// Marks `OwnerEdited` on the files of a diff page that the owner edited before a turn
/// end settled the edit on this change set, by path or, for a rename, old path. Inputs:
/// the store, the worktree, the change set and the files of the page. Output: Ok once
/// marked.
#[cfg(unix)]
pub(super) fn mark(
    db: &Connection,
    worktree: &Path,
    change_set: &super::ChangeSetId,
    files: &mut [super::ChangedFile],
) -> Result<(), EngineError> {
    use super::FileMark;
    let worktree = worktree.to_str().ok_or_else(invalid)?;
    db.execute_batch(SCHEMA).map_err(storage)?;
    let mut statement = db
        .prepare(
            "SELECT EXISTS(SELECT 1 FROM landing_owner_edits
             WHERE worktree=?1 AND change_set=?2 AND path=?3)",
        )
        .map_err(storage)?;
    let mut edited = |path: &str| {
        statement
            .query_row(params![worktree, change_set.as_str(), path], |row| {
                row.get::<_, bool>(0)
            })
            .map_err(storage)
    };
    for file in files {
        let touched = edited(file.path.as_str())?
            || match &file.old_path {
                Some(old) => edited(old.as_str())?,
                None => false,
            };
        if touched && !file.marks.contains(&FileMark::OwnerEdited) {
            file.marks.push(FileMark::OwnerEdited);
        }
    }
    Ok(())
}

fn invalid() -> EngineError {
    EngineError("changes_path_invalid".into())
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    EngineError("changes_unavailable".into())
}

#[cfg(all(test, unix))]
mod tests;
