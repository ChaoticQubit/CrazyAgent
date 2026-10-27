use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// The most owner-edited paths kept for one agent worktree.
pub const PATHS_PER_WORKTREE_MAX: i64 = 1000;
/// The most bytes an owner-edited path may hold.
pub const PATH_BYTES_MAX: usize = 4096;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS landing_owner_edits (
    worktree TEXT NOT NULL,
    path TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY (worktree, path)
);";

/// Records that the owner edited `path` in an agent's worktree at `at`, so the change set
/// that holds the edit, the one open then or the next one the worktree opens, marks the
/// file `OwnerEdited`. Inputs: the store, the agent's worktree, the path relative to it
/// and the time in ms. Output: Ok once kept, `changes_path_invalid` for a path outside the
/// worktree or `changes_owner_edits_full` past `PATHS_PER_WORKTREE_MAX` paths.
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
            "SELECT at_ms FROM landing_owner_edits WHERE worktree=?1 AND path=?2",
            params![worktree, path],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    if known.is_none() {
        let kept: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM landing_owner_edits WHERE worktree=?1",
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
         ON CONFLICT(worktree, path) DO UPDATE SET at_ms=excluded.at_ms",
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

/// Marks `OwnerEdited` on the files the owner edited for one change set: edits in its
/// worktree after the change set the worktree closed before it opened, up to its own
/// close. Inputs: the store, the worktree, when the change set opened, when it closed
/// (none while open) and the files of a diff page. Output: Ok once marked.
#[cfg(unix)]
pub(super) fn mark(
    db: &Connection,
    worktree: &Path,
    created_ms: u64,
    closed_ms: Option<u64>,
    files: &mut [super::ChangedFile],
) -> Result<(), EngineError> {
    use super::FileMark;
    use std::collections::BTreeSet;
    let worktree = worktree.to_str().ok_or_else(invalid)?;
    let signed = |ms: u64| i64::try_from(ms).map_err(|_| invalid());
    let (created_ms, closed_ms) = (signed(created_ms)?, closed_ms.map(signed).transpose()?);
    db.execute_batch(SCHEMA).map_err(storage)?;
    let after: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(updated_ms), 0) FROM landing_change_sets
             WHERE worktree=?1 AND state IN ('accepted','discarded') AND created_ms<?2",
            params![worktree, created_ms],
            |row| row.get(0),
        )
        .map_err(storage)?;
    let mut statement = db
        .prepare_cached(
            "SELECT path FROM landing_owner_edits
             WHERE worktree=?1 AND at_ms>?2 AND at_ms<=?3 LIMIT ?4",
        )
        .map_err(storage)?;
    let edited: BTreeSet<String> = statement
        .query_map(
            params![
                worktree,
                after,
                closed_ms.unwrap_or(i64::MAX),
                PATHS_PER_WORKTREE_MAX
            ],
            |row| row.get(0),
        )
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    for file in files {
        let touched = edited.contains(&file.path)
            || file
                .old_path
                .as_ref()
                .is_some_and(|old| edited.contains(old));
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
