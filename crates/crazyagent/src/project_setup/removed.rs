use super::{error, failure, now_ms};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::state_log::fence::Fence;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

/// The `ProjectRemoved` event a removal appends to the state log, and its projection
/// (v2.5 L14a).
pub(crate) mod event;
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
#[cfg(unix)]
const ID_BYTES_MAX: usize = 128;

/// The longest id onboarding gives a folder, suffix included, as `project_id_for` does.
const SLUG_BYTES_MAX: usize = 64;

/// Creates the removal record: one row per removed project id, with the folder CrazyAgent
/// recorded for it and whether adding that exact folder again brings the project back.
/// Input: the engine connection. Output: Ok or the database's failure.
#[cfg(unix)]
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
                ON projects_removed(folder) WHERE returns=1;
            CREATE TABLE IF NOT EXISTS projects_removed_backfill (
                only INTEGER PRIMARY KEY CHECK(only=1),
                done_ms INTEGER NOT NULL
            );"
        ))
        .map_err(error)?;
    Ok(())
}

/// The tables with a `project` column, read from the database itself, so a table another
/// line's build made counts too; `projects_removed` and names that are not plain ASCII
/// letters, digits and `_` are left out, since the names are written into SQL. Input: the
/// connection. Output: at most `TABLES_MAX` names.
fn keyed_tables(connection: &Connection) -> Result<Vec<String>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT m.name FROM sqlite_schema m
             WHERE m.type='table' AND m.name<>'projects_removed'
             AND EXISTS (SELECT 1 FROM pragma_table_info(m.name) c WHERE c.name='project')
             ORDER BY m.name LIMIT ?1",
        )
        .map_err(error)?;
    let names = statement
        .query_map([TABLES_MAX], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let names: Vec<String> = names
        .into_iter()
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .collect();
    assert!(i64::try_from(names.len()).is_ok_and(|count| count <= TABLES_MAX));
    Ok(names)
}

/// Holds, as not returnable, every id that is not a live project and still has a row in a
/// table with a `project` column, tables this build does not know included, so no removal
/// made before this record existed gives its id out again. Runs once per database, at its
/// first open, and never fills the record past `REMOVED_MAX`. Input: the engine connection.
/// Output: how many ids it held, None when an earlier open already ran it.
#[cfg(unix)]
pub(crate) fn hold_left(connection: &Connection) -> Result<Option<usize>, EngineError> {
    let tx = connection.unchecked_transaction().map_err(error)?;
    let done: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects_removed_backfill)",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if done {
        return Ok(None);
    }
    let now = now_ms();
    let mut held = 0;
    for table in keyed_tables(&tx)? {
        let recorded: i64 = tx
            .query_row("SELECT count(*) FROM projects_removed", [], |row| {
                row.get(0)
            })
            .map_err(error)?;
        let room = REMOVED_MAX - recorded;
        if room <= 0 {
            eprintln!("projects_removed_backfill_bound held={held}");
            break;
        }
        held += tx
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO projects_removed(project,folder,returns,removed_ms)
                     SELECT DISTINCT project, NULL, 0, ?1 FROM \"{table}\"
                     WHERE typeof(project)='text'
                     AND length(project) BETWEEN 1 AND {ID_BYTES_MAX}
                     AND project <> ?3
                     AND lower(project) NOT IN
                        (SELECT lower(id) FROM projects WHERE id IS NOT NULL)
                     AND lower(project) NOT IN (SELECT lower(project) FROM projects_removed)
                     LIMIT ?2"
                ),
                params![now, room, crate::usage::REMOVED_PROJECTS],
            )
            .map_err(error)?;
    }
    tx.execute(
        "INSERT INTO projects_removed_backfill(only,done_ms) VALUES (1,?1)",
        [now],
    )
    .map_err(error)?;
    tx.commit().map_err(error)?;
    assert!(i64::try_from(held).is_ok_and(|count| count <= REMOVED_MAX));
    Ok(Some(held))
}

/// Whether one more removal fits the record. Input: the connection. Output: Ok, or
/// `projects_removed_full` once `REMOVED_MAX` removals are recorded.
pub(crate) fn room(connection: &Connection) -> Result<Result<(), &'static str>, EngineError> {
    let recorded: i64 = connection
        .query_row("SELECT count(*) FROM projects_removed", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    assert!(recorded >= 0);
    Ok(if recorded >= REMOVED_MAX {
        Err("projects_removed_full")
    } else {
        Ok(())
    })
}

/// Records one removal: the project, the folder CrazyAgent recorded for it and whether
/// adding that folder again brings it back. Never refuses, so a Delete everything whose
/// folders already went to the Trash is always recorded. Inputs: the transaction, the
/// project, its folder and whether it may come back. Output: Ok or the database's failure.
pub(crate) fn record(
    connection: &Connection,
    project: &ProjectId,
    folder: Option<&str>,
    returns: bool,
) -> Result<(), EngineError> {
    let folder = folder.filter(|text| (1..=FOLDER_BYTES_MAX).contains(&text.len()));
    let returns = returns && folder.is_some();
    if returns {
        connection
            .execute(
                "UPDATE projects_removed SET returns=0 WHERE folder=?1 AND returns=1",
                [folder],
            )
            .map_err(error)?;
    }
    connection
        .execute(
            "INSERT INTO projects_removed(project,folder,returns,removed_ms) VALUES (?1,?2,?3,?4)
             ON CONFLICT(project) DO UPDATE SET
                folder=excluded.folder, returns=excluded.returns, removed_ms=excluded.removed_ms",
            params![project.as_str(), folder, returns, now_ms()],
        )
        .map_err(error)?;
    assert!(held(connection, project)?);
    Ok(())
}

/// The project removed from the dashboard whose recorded folder is exactly this text.
/// Inputs: the connection and the canonical folder text. Output: its id, or None.
pub(crate) fn returning(
    connection: &Connection,
    folder: &str,
) -> Result<Option<ProjectId>, EngineError> {
    let found: Option<String> = connection
        .query_row(
            "SELECT project FROM projects_removed WHERE folder=?1 AND returns=1",
            [folder],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    found
        .map(|text| ProjectId::new(text).map_err(|problem| failure(&problem.0)))
        .transpose()
}

/// Whether a removal holds this id, ASCII case ignored. Inputs: the connection and the id.
/// Output: the answer.
pub(crate) fn held(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects_removed WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Marks a project recorded at this folder as live again: its own returnable row goes, and
/// any other id's return to the same folder ends, so that id stays held. Inputs: the
/// transaction, the project and its folder text. Output: Ok or the database's failure.
pub(crate) fn claim(
    connection: &Connection,
    project: &ProjectId,
    folder: &str,
) -> Result<(), EngineError> {
    connection
        .execute(
            "DELETE FROM projects_removed WHERE project=?1 AND folder=?2 AND returns=1",
            params![project.as_str(), folder],
        )
        .map_err(error)?;
    connection
        .execute(
            "UPDATE projects_removed SET returns=0 WHERE folder=?1 AND returns=1",
            [folder],
        )
        .map_err(error)?;
    assert!(returning(connection, folder)?.is_none());
    Ok(())
}

/// The id a folder with no project of its own is onboarded as: its name's id, or the first
/// of `-2` to `-99` after it that no removal holds, no project uses and no leftover row
/// keeps. A live id here is always another folder's, since a folder's own project is
/// answered before an id is picked. Inputs: the connection and the folder. Output: the id,
/// or `onboarding_project_ids_used` when every one is taken.
pub(crate) fn free_id(
    connection: &Connection,
    folder: &Path,
) -> Result<Result<ProjectId, &'static str>, EngineError> {
    let base = crate::onboarding::project_id_for(folder)?;
    assert!(base.as_str().len() <= SLUG_BYTES_MAX && base.as_str().is_ascii());
    for n in 1..=SUFFIX_MAX {
        let candidate = if n == 1 {
            base.clone()
        } else {
            suffixed(&base, n)?
        };
        if held(connection, &candidate)? {
            continue;
        }
        let live: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE lower(id)=lower(?1))",
                [candidate.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !live && !left(connection, &candidate)? {
            return Ok(Ok(candidate));
        }
    }
    Ok(Err("onboarding_project_ids_used"))
}

/// Whether a table with a `project` column still has a row of this id, so a removal made
/// after the backfill by a build without the removal record never gives its id out. Each
/// table is read by the exact id first, through its index where it has one; every row is
/// read case-insensitively only where a row under another case can exist ([`other_case`]).
/// The onboarding table is left out: starting onboarding replaces or refuses its row itself.
/// An id a live project has in any case answers false, its rows being that project's.
/// Inputs: the connection and the id. Output: the answer; nothing is written.
pub(crate) fn left(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    let live: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE lower(id)=lower(?1))",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if live {
        return Ok(false);
    }
    let tables: Vec<String> = keyed_tables(connection)?
        .into_iter()
        .filter(|table| table != "onboarding")
        .collect();
    let found = |test: &str| -> Result<bool, EngineError> {
        for table in &tables {
            let hit: bool = connection
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE {test})"),
                    [project.as_str()],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    };
    if found("project=?1")? {
        return Ok(true);
    }
    if !other_case(connection, project)? {
        return Ok(false);
    }
    found("lower(project)=lower(?1)")
}

/// Whether a row under another case of this id can exist: the id has capitals, or a live or
/// removed project's id has. Only CreateProject takes an id with capitals, and every row of a
/// project carries its id as given, so with neither the exact read in [`left`] is complete.
/// Inputs: the connection and the id. Output: the answer.
fn other_case(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    if project.as_str() != project.as_str().to_ascii_lowercase() {
        return Ok(true);
    }
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id COLLATE BINARY <> lower(id))
                OR EXISTS(SELECT 1 FROM projects_removed
                          WHERE project COLLATE BINARY <> lower(project))",
            [],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Whether a live project's own folder is exactly this folder text, as a removal records it
/// and [`returning`] compares it, so adding a project's own folder never makes a second
/// project in it, whether onboarding, New Project or Create demo made the first (rv-code4
/// r2 F1). Inputs: the connection and the folder text. Output: the answer; it reads one
/// setup per live project.
pub(crate) fn live_at(connection: &Connection, folder: &str) -> Result<bool, EngineError> {
    let mut statement = connection
        .prepare("SELECT s.body FROM project_setups s JOIN projects p ON p.id=s.project")
        .map_err(error)?;
    let bodies = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?;
    for body in bodies {
        let stored: super::StoredSetup =
            serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        if stored.project_folder().to_str() == Some(folder) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The id `base-n`, the base cut so the id stays within `SLUG_BYTES_MAX` bytes, with no dash
/// left at the cut. Inputs: an onboarding id and n from 2 to `SUFFIX_MAX`. Output: the id.
fn suffixed(base: &ProjectId, n: u32) -> Result<ProjectId, EngineError> {
    assert!((2..=SUFFIX_MAX).contains(&n));
    let suffix = format!("-{n}");
    let mut stem = base.as_str().to_owned();
    stem.truncate(SLUG_BYTES_MAX - suffix.len());
    while stem.ends_with('-') {
        stem.pop();
    }
    if stem.is_empty() {
        stem.push_str("project");
    }
    let id = format!("{stem}{suffix}");
    assert!(id.len() <= SLUG_BYTES_MAX);
    ProjectId::new(id).map_err(|problem| failure(&problem.0))
}

/// Moves this id's token counts to Removed projects, forgets its ide rows and tombstones its
/// ide events, repository admissions (rv-sec2 L7 r2 N1, #345 N4) and landing events (L9
/// QUESTION-1046), then deletes every other row of it in every table keyed by project, its
/// removal record included, so a demo made again inherits nothing of an earlier one. Inputs:
/// the transaction, the id and the owner's fence. Output: Ok, or a store or fence refusal.
pub(crate) fn clear(
    tx: &Transaction<'_>,
    project: &ProjectId,
    fence: &Fence,
) -> Result<(), EngineError> {
    assert!(!tx.is_autocommit(), "the clear is part of the Create demo");
    crate::usage::forget_project(tx, project.as_str())?;
    crate::ide::log::forget_rows(tx, project)?;
    crate::ide::log::forget_project(tx, project.as_str(), None, fence)?;
    crate::worktrees::log::forget_project(tx, project.as_str(), None, fence)?;
    crate::landing_log::forget_project(tx, project.as_str(), fence)?;
    for table in keyed_tables(tx)? {
        tx.execute(
            &format!("DELETE FROM \"{table}\" WHERE project=?1"),
            [project.as_str()],
        )
        .map_err(error)?;
    }
    tx.execute(
        "DELETE FROM projects_removed WHERE project=?1",
        [project.as_str()],
    )
    .map_err(error)?;
    assert!(!held(tx, project)?);
    Ok(())
}
