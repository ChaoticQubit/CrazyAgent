use super::*;
use crate::migrations::{BACKUPS, DATABASE};
use std::os::unix::fs::PermissionsExt;

/// A step's result with the step named in its error, for row fixtures. Inputs: the result
/// and the step. Output: the value, or the named error.
fn named<T, E: std::fmt::Display>(
    result: Result<T, E>,
    step: impl std::fmt::Display,
) -> Result<T, String> {
    result.map_err(|error| format!("{step}: {error}"))
}

/// No settings texts. Input: none. Output: an empty list.
fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// A database in a private folder, holding one table of one row when `filled`. Input:
/// whether to fill it. Output: the folder, the database path and its connection, or the
/// step and path that failed.
fn database(filled: bool) -> Result<(tempfile::TempDir, PathBuf, Connection), String> {
    let root = named(tempfile::tempdir(), "make a temporary folder")?;
    named(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        format!("set the mode of {:?}", root.path()),
    )?;
    let path = root.path().join(DATABASE);
    let db = named(Connection::open(&path), format!("open {path:?}"))?;
    if filled {
        named(
            db.execute_batch("CREATE TABLE kept (value TEXT); INSERT INTO kept VALUES ('one');"),
            format!("fill {path:?}"),
        )?;
    }
    Ok((root, path, db))
}

/// Steps numbered from 2 in the order given. Input: their bodies. Output: the steps.
fn steps(runs: &[StepRun]) -> Vec<Step> {
    (2..)
        .zip(runs)
        .map(|(to, run)| Step { to, run: *run })
        .collect()
}

fn added(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch("CREATE TABLE added (a)").map_err(error)
}

fn empty(_tx: &Transaction<'_>) -> Result<(), StorageError> {
    Ok(())
}

fn broken(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch("CREATE TABLE broken (").map_err(error)
}

/// A step that ends the upgrade transaction with no transaction statement: SQLite itself
/// rolls it back on a duplicate under `OR ROLLBACK`, the step swallows that error, then
/// deletes and creates a table in autocommit, so the backup has committed work to undo.
fn escaping(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch(
        "DELETE FROM kept; CREATE TABLE escaped (a UNIQUE); INSERT INTO escaped VALUES (1);",
    )
    .map_err(error)?;
    let duplicate = tx.execute("INSERT OR ROLLBACK INTO escaped VALUES (1)", []);
    assert!(
        duplicate.is_err(),
        "the duplicate rolls the transaction back"
    );
    tx.execute_batch("DELETE FROM kept; CREATE TABLE escaped_after (a);")
        .map_err(error)?;
    Ok(())
}

fn commits_and_begins(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch("DELETE FROM kept; COMMIT; BEGIN;")
        .map_err(error)
}

fn commits(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch("DELETE FROM kept; COMMIT;").map_err(error)
}

fn savepoints(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch("SAVEPOINT inner; DELETE FROM kept; RELEASE inner;")
        .map_err(error)
}

/// Runs an upgrade whose second step tries a transaction statement and checks it is
/// refused before it runs, so the upgrade fails, the step's earlier delete rolls back with
/// the transaction, and the runner's hook is gone after. Input: that step. Output: Ok, or
/// the fixture step that failed.
fn refused_unchanged(step: StepRun) -> Result<(), String> {
    let (root, path, mut db) = database(true)?;
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, step]))
        .unwrap_err()
        .0;
    assert!(
        failed.starts_with("storage_migration_failed: step 3 failed: "),
        "{failed}"
    );
    assert!(failed.contains("not authorized"), "{failed}");
    assert!(failed.contains("; the database is unchanged; "), "{failed}");
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(!holds(&db, "added")?);
    assert_eq!(kept(&db)?, ["one"]);
    assert_eq!(backups(root.path())?.len(), 1);
    named(db.execute_batch("BEGIN; ROLLBACK;"), "begin and roll back")?;
    Ok(())
}

/// Whether the database holds a table. Inputs: the connection and the name. Output: the
/// answer, or the read's failure.
fn holds(db: &Connection, table: &str) -> Result<bool, String> {
    named(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        ),
        format!("read whether {table} exists"),
    )
}

/// The values of the kept table. Input: the connection. Output: them, in order, or the
/// read's failure.
fn kept(db: &Connection) -> Result<Vec<String>, String> {
    named(
        db.prepare("SELECT value FROM kept ORDER BY value")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<_, _>>()
            }),
        "read the kept table",
    )
}

/// The backup folders an upgrade left; none when the backups folder was never made. Input:
/// the database folder. Output: their paths, or the listing's failure.
fn backups(root: &Path) -> Result<Vec<PathBuf>, String> {
    let folder = root.join(BACKUPS);
    match std::fs::read_dir(&folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        listed => named(listed, format!("list {folder:?}"))?
            .map(|entry| named(entry, format!("an entry in {folder:?}")).map(|entry| entry.path()))
            .collect(),
    }
}

#[test]
#[should_panic(expected = "migration steps are numbered from 2 without gaps")]
fn steps_must_be_numbered_from_two_without_gaps() {
    latest(&[Step { to: 3, run: added }]);
}

#[test]
fn an_empty_database_is_stamped_newest_without_a_backup_or_a_step() -> Result<(), String> {
    let (root, path, mut db) = database(false)?;
    named(
        upgrade(&mut db, &path, &no_texts, &steps(&[added, added])),
        "upgrade",
    )?;
    assert_eq!(named(version(&db), "read the version")?, 3);
    assert!(!holds(&db, "added")?);
    assert!(backups(root.path())?.is_empty());
    Ok(())
}

#[test]
fn a_database_a_newer_engine_wrote_is_refused_and_never_written() -> Result<(), String> {
    let (root, path, mut db) = database(true)?;
    named(
        db.execute_batch("PRAGMA user_version = 9"),
        "set the version",
    )?;
    let refused = upgrade(&mut db, &path, &no_texts, &steps(&[added]))
        .unwrap_err()
        .0;
    assert!(
        refused.starts_with("storage_newer_than_engine: "),
        "{refused}"
    );
    assert!(check(&db, &steps(&[added])).is_err());
    assert_eq!(named(version(&db), "read the version")?, 9);
    assert!(!holds(&db, "added")?);
    assert!(backups(root.path())?.is_empty());
    Ok(())
}

#[test]
fn check_refuses_an_old_database_until_the_upgrade_brings_it_to_the_newest() -> Result<(), String> {
    let (root, path, mut db) = database(true)?;
    let all = steps(&[added, added]);
    let refused = check(&db, &all).unwrap_err().0;
    assert!(
        refused.starts_with("storage_upgrade_required: "),
        "{refused}"
    );
    named(
        upgrade(&mut db, &path, &no_texts, &steps(&[added, empty])),
        "upgrade",
    )?;
    named(check(&db, &all), "check")?;
    named(db.execute_batch("BEGIN; COMMIT;"), "begin and commit")?;
    assert_eq!(named(version(&db), "read the version")?, 3);
    assert!(holds(&db, "added")?);
    assert_eq!(backups(root.path())?.len(), 1);
    Ok(())
}

#[test]
fn the_real_upgrade_refuses_a_copy_the_live_database_outgrew_and_runs_no_step() -> Result<(), String>
{
    let (root, path, mut db) = database(true)?;
    let writer = named(Connection::open(&path), format!("open {path:?}"))?;
    let grows = || {
        writer
            .execute("INSERT INTO kept VALUES ('two')", [])
            .map_err(error)?;
        no_texts()
    };
    let refused = upgrade(&mut db, &path, &grows, &steps(&[added]))
        .unwrap_err()
        .0;
    assert_eq!(
        refused,
        "storage_backup_failed: the backup copy holds 1 rows in kept where engine.db holds 2"
    );
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(!holds(&db, "added")?);
    assert_eq!(kept(&db)?, ["one", "two"]);
    assert!(backups(root.path())?.is_empty());
    Ok(())
}

#[test]
fn a_failed_step_rolls_back_and_names_the_backup() -> Result<(), String> {
    let (root, path, mut db) = database(true)?;
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, broken]))
        .unwrap_err()
        .0;
    let made = backups(root.path())?;
    assert_eq!(made.len(), 1);
    assert!(
        failed.starts_with("storage_migration_failed: step 3 failed: "),
        "{failed}"
    );
    assert!(failed.contains("; the database is unchanged; "), "{failed}");
    assert!(
        failed.ends_with(&format!("backup at {}", made[0].display())),
        "{failed}"
    );
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(!holds(&db, "added")?);
    assert_eq!(kept(&db)?, ["one"]);
    Ok(())
}

#[test]
fn a_step_that_ends_the_transaction_is_restored_from_the_backup() -> Result<(), String> {
    let (root, path, mut db) = database(true)?;
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, escaping]))
        .unwrap_err()
        .0;
    let made = backups(root.path())?;
    assert_eq!(made.len(), 1);
    assert!(
        failed.starts_with(
            "storage_migration_failed: step 3 failed: the step ended the upgrade transaction; "
        ),
        "{failed}"
    );
    assert!(
        failed.contains("; the database was restored from the backup; "),
        "{failed}"
    );
    assert!(
        failed.ends_with(&format!("backup at {}", made[0].display())),
        "{failed}"
    );
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(!holds(&db, "added")? && !holds(&db, "escaped")?);
    assert!(
        !holds(&db, "escaped_after")?,
        "the committed table is undone"
    );
    assert_eq!(kept(&db)?, ["one"]);
}

#[test]
fn a_step_that_commits_and_begins_again_is_refused_and_changes_nothing() -> Result<(), String> {
    refused_unchanged(commits_and_begins)
}

#[test]
fn a_step_that_commits_is_refused_and_changes_nothing() -> Result<(), String> {
    refused_unchanged(commits)
}

#[test]
fn a_step_that_opens_a_savepoint_is_refused_and_changes_nothing() -> Result<(), String> {
    refused_unchanged(savepoints)
}

/// Leaves the restore record `line` beside the database. Inputs: the root and the line.
/// Output: Ok, or the write's failure.
fn recorded(root: &Path, line: &str) -> Result<(), String> {
    named(
        crate::restore::write_restore_record(root, line),
        format!("write the restore record in {root:?}"),
    )
}

#[test]
fn a_start_after_a_failed_rollback_refuses_and_leaves_engine_db_unmigrated() -> Result<(), String> {
    use crate::restore::{ENGINE_RESTORE_BROKEN, RECORD_FAILED, RESTORE_ROLLBACK_FAILED};
    let (root, path, mut db) = database(true)?;
    let line = format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED}");
    recorded(root.path(), &line)?;
    let before = named(std::fs::read(&path), format!("read {path:?}"))?;

    let refused = upgrade(&mut db, &path, &no_texts, &steps(&[added]))
        .unwrap_err()
        .0;

    assert!(refused.starts_with(ENGINE_RESTORE_BROKEN), "{refused}");
    assert!(refused.contains("backups"), "{refused}");
    assert!(!holds(&db, "added")?);
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(backups(root.path())?.is_empty());
    assert_eq!(
        named(std::fs::read(&path), format!("read {path:?}"))?,
        before
    );
    assert_eq!(
        crate::restore::read_restore_record(root.path()).as_deref(),
        Some(line.as_str())
    );
    Ok(())
}

/// A record left at `restoring`, by a crash or a failed
/// outcome write after the restore started, refuses the next start without migrating, and
/// the start keeps the record.
#[test]
fn a_start_after_a_restore_left_at_restoring_refuses_and_leaves_engine_db_unmigrated()
-> Result<(), String> {
    use crate::restore::{ENGINE_RESTORE_BROKEN, RECORD_RESTORING};
    let (root, path, mut db) = database(true)?;
    recorded(root.path(), RECORD_RESTORING)?;
    let before = named(std::fs::read(&path), format!("read {path:?}"))?;

    let refused = upgrade(&mut db, &path, &no_texts, &steps(&[added]))
        .unwrap_err()
        .0;

    assert!(refused.starts_with(ENGINE_RESTORE_BROKEN), "{refused}");
    assert!(!holds(&db, "added")?);
    assert_eq!(named(version(&db), "read the version")?, 0);
    assert!(backups(root.path())?.is_empty());
    assert_eq!(
        named(std::fs::read(&path), format!("read {path:?}"))?,
        before
    );
    assert_eq!(
        crate::restore::read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
    Ok(())
}

#[test]
fn a_start_after_a_restore_that_rolled_back_still_migrates() -> Result<(), String> {
    use crate::restore::{RECORD_FAILED, RESTORE_FAILED};
    let (root, path, mut db) = database(true)?;
    let line = format!("{RECORD_FAILED} {RESTORE_FAILED}");
    recorded(root.path(), &line)?;

    named(
        upgrade(&mut db, &path, &no_texts, &steps(&[added])),
        "upgrade",
    )?;

    assert!(holds(&db, "added")?);
    assert_eq!(named(version(&db), "read the version")?, 2);
    assert_eq!(
        crate::restore::read_restore_record(root.path()).as_deref(),
        Some(line.as_str())
    );
    Ok(())
}
