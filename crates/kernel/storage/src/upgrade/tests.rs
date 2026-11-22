use super::*;
use crate::migrations::{BACKUPS, DATABASE};
use std::os::unix::fs::PermissionsExt;

/// No settings texts. Input: none. Output: an empty list.
fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// A database in a private folder, holding one table of one row when `filled`. Input:
/// whether to fill it. Output: the folder, the database path and its connection.
fn database(filled: bool) -> (tempfile::TempDir, PathBuf, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join(DATABASE);
    let db = Connection::open(&path).unwrap();
    if filled {
        db.execute_batch("CREATE TABLE kept (value TEXT); INSERT INTO kept VALUES ('one');")
            .unwrap();
    }
    (root, path, db)
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
/// the transaction, and the runner's hook is gone after. Input: that step. Output: none.
fn refused_unchanged(step: StepRun) {
    let (root, path, mut db) = database(true);
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, step]))
        .unwrap_err()
        .0;
    assert!(
        failed.starts_with("storage_migration_failed: step 3 failed: "),
        "{failed}"
    );
    assert!(failed.contains("not authorized"), "{failed}");
    assert!(failed.contains("; the database is unchanged; "), "{failed}");
    assert_eq!(version(&db).unwrap(), 0);
    assert!(!holds(&db, "added"));
    assert_eq!(kept(&db), ["one"]);
    assert_eq!(backups(root.path()).len(), 1);
    db.execute_batch("BEGIN; ROLLBACK;").unwrap();
}

/// Whether the database holds a table. Inputs: the connection and the name. Output: the answer.
fn holds(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

/// The values of the kept table. Input: the connection. Output: them, in order.
fn kept(db: &Connection) -> Vec<String> {
    db.prepare("SELECT value FROM kept ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The backup folders an upgrade left. Input: the database folder. Output: their paths.
fn backups(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join(BACKUPS)).map_or_else(
        |_| Vec::new(),
        |entries| entries.map(|entry| entry.unwrap().path()).collect(),
    )
}

#[test]
#[should_panic(expected = "migration steps are numbered from 2 without gaps")]
fn steps_must_be_numbered_from_two_without_gaps() {
    latest(&[Step { to: 3, run: added }]);
}

#[test]
fn an_empty_database_is_stamped_newest_without_a_backup_or_a_step() {
    let (root, path, mut db) = database(false);
    upgrade(&mut db, &path, &no_texts, &steps(&[added, added])).unwrap();
    assert_eq!(version(&db).unwrap(), 3);
    assert!(!holds(&db, "added"));
    assert!(backups(root.path()).is_empty());
}

#[test]
fn a_database_a_newer_engine_wrote_is_refused_and_never_written() {
    let (root, path, mut db) = database(true);
    db.execute_batch("PRAGMA user_version = 9").unwrap();
    let refused = upgrade(&mut db, &path, &no_texts, &steps(&[added]))
        .unwrap_err()
        .0;
    assert!(
        refused.starts_with("storage_newer_than_engine: "),
        "{refused}"
    );
    assert!(check(&db, &steps(&[added])).is_err());
    assert_eq!(version(&db).unwrap(), 9);
    assert!(!holds(&db, "added"));
    assert!(backups(root.path()).is_empty());
}

#[test]
fn check_refuses_an_old_database_until_the_upgrade_brings_it_to_the_newest() {
    let (root, path, mut db) = database(true);
    let all = steps(&[added, added]);
    let refused = check(&db, &all).unwrap_err().0;
    assert!(
        refused.starts_with("storage_upgrade_required: "),
        "{refused}"
    );
    upgrade(&mut db, &path, &no_texts, &steps(&[added, empty])).unwrap();
    check(&db, &all).unwrap();
    db.execute_batch("BEGIN; COMMIT;").unwrap();
    assert_eq!(version(&db).unwrap(), 3);
    assert!(holds(&db, "added"));
    assert_eq!(backups(root.path()).len(), 1);
}

#[test]
fn the_real_upgrade_refuses_a_copy_the_live_database_outgrew_and_runs_no_step() {
    let (root, path, mut db) = database(true);
    let writer = Connection::open(&path).unwrap();
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
    assert_eq!(version(&db).unwrap(), 0);
    assert!(!holds(&db, "added"));
    assert_eq!(kept(&db), ["one", "two"]);
    assert!(backups(root.path()).is_empty());
}

#[test]
fn a_failed_step_rolls_back_and_names_the_backup() {
    let (root, path, mut db) = database(true);
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, broken]))
        .unwrap_err()
        .0;
    let made = backups(root.path());
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
    assert_eq!(version(&db).unwrap(), 0);
    assert!(!holds(&db, "added"));
    assert_eq!(kept(&db), ["one"]);
}

#[test]
fn a_step_that_ends_the_transaction_is_restored_from_the_backup() {
    let (root, path, mut db) = database(true);
    let failed = upgrade(&mut db, &path, &no_texts, &steps(&[added, escaping]))
        .unwrap_err()
        .0;
    let made = backups(root.path());
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
    assert_eq!(version(&db).unwrap(), 0);
    assert!(!holds(&db, "added") && !holds(&db, "escaped"));
    assert!(
        !holds(&db, "escaped_after"),
        "the committed table is undone"
    );
    assert_eq!(kept(&db), ["one"]);
}

#[test]
fn a_step_that_commits_and_begins_again_is_refused_and_changes_nothing() {
    refused_unchanged(commits_and_begins);
}

#[test]
fn a_step_that_commits_is_refused_and_changes_nothing() {
    refused_unchanged(commits);
}

#[test]
fn a_step_that_opens_a_savepoint_is_refused_and_changes_nothing() {
    refused_unchanged(savepoints);
}
