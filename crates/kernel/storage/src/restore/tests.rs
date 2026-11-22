use super::*;
use crate::migrations::{BACKUPS, DATABASE, backup};
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

/// No settings texts. Input: none. Output: an empty list.
fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// No agent runs. Input: the connection. Output: false.
fn idle(_: &Connection) -> Result<bool, StorageError> {
    Ok(false)
}

/// An engine root in a canonical private folder whose engine.db was upgraded from 2.4: a
/// checked `11-to-12` backup holds `kept` = ["one"] at version 11, and engine.db now holds
/// ["two"] at version 12. Input: whether to take the pinned backup. Output: the root and
/// the database path.
fn upgraded(pinned: bool) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = root.path().join(DATABASE);
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE kept (value TEXT); INSERT INTO kept VALUES ('one'); \
         PRAGMA user_version = 11;",
    )
    .unwrap();
    if pinned {
        backup(&db, &path, &no_texts, (11, 12), SystemTime::now()).unwrap();
    }
    db.execute_batch(
        "DELETE FROM kept; INSERT INTO kept VALUES ('two'); PRAGMA user_version = 12;",
    )
    .unwrap();
    drop(db);
    (root, path)
}

/// The values in `kept` and the version of a database. Input: its path. Output: both.
fn read(path: &Path) -> (Vec<String>, i64) {
    let db = Connection::open(path).unwrap();
    let values = db
        .prepare("SELECT value FROM kept ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let version = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    (values, version)
}

/// The dated copies a restore kept of the data it replaced. Input: the engine root.
/// Output: their folders.
fn replaced(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join(BACKUPS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("replaced-"))
        })
        .collect()
}

/// Runs a restore whose busy hook answers `busy` and checks it is refused with `code`
/// before any copy: engine.db byte-equal, no replaced copy and no after-restore call.
/// Inputs: the hook's answer and the code. Output: none.
fn refused_before_any_copy(busy: Result<bool, StorageError>, code: &str) {
    let (root, path) = upgraded(true);
    let before = std::fs::read(&path).unwrap();
    let after = Cell::new(0);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &|_| busy.clone(),
        after_restore: &|_, _, _| after.set(after.get() + 1),
    };

    let refused = restore_pinned(&path, &hooks).unwrap_err().0;

    assert_eq!(refused, code);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(replaced(root.path()).is_empty());
    assert_eq!(after.get(), 0);
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
}

#[test]
fn a_restore_while_an_agent_runs_is_refused_before_any_copy() {
    refused_before_any_copy(Ok(true), RESTORE_AGENTS_RUNNING);
}

#[test]
fn a_busy_hook_that_fails_refuses_the_restore_before_any_copy() {
    refused_before_any_copy(Err(StorageError("probe_failed".to_owned())), "probe_failed");
}

#[test]
fn a_restore_while_another_connection_holds_the_database_is_refused_as_busy() {
    let (root, path) = upgraded(true);
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let asked = Cell::new(0);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &|_| {
            asked.set(asked.get() + 1);
            Ok(false)
        },
        after_restore: &|_, _, _| {},
    };

    let refused = restore_pinned(&path, &hooks).unwrap_err().0;

    assert_eq!(refused, RESTORE_BUSY);
    assert_eq!(asked.get(), 0);
    assert!(replaced(root.path()).is_empty());
    holder.execute_batch("ROLLBACK;").unwrap();
}

#[test]
fn with_no_pinned_backup_the_restore_is_unavailable_and_asks_nothing() {
    let (root, path) = upgraded(false);
    let asked = Cell::new(0);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &|_| {
            asked.set(asked.get() + 1);
            Ok(false)
        },
        after_restore: &|_, _, _| {},
    };

    let refused = restore_pinned(&path, &hooks).unwrap_err().0;

    assert_eq!(refused, RESTORE_UNAVAILABLE);
    assert_eq!(asked.get(), 0);
    assert_eq!(restorable_until(root.path(), SystemTime::now()), None);
}

#[test]
fn a_restore_brings_the_2_4_data_back_and_keeps_the_replaced_data() {
    let (root, path) = upgraded(true);
    let now = SystemTime::now();
    assert!(restorable_until(root.path(), now).is_some_and(|until| until > now));
    let texts = || Ok(vec![("settings.json", "{\"a\": 1}".to_owned())]);
    let swept = Cell::new(0);
    let hooks = Hooks {
        texts: &texts,
        busy: &idle,
        after_restore: &|db, swept_root, kept| {
            assert_eq!(swept_root, root.path());
            assert!(kept.starts_with(root.path().join(BACKUPS)));
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                11
            );
            swept.set(swept.get() + 1);
        },
    };

    let kept = restore_pinned(&path, &hooks).unwrap();

    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
    assert_eq!(replaced(root.path()), std::slice::from_ref(&kept));
    assert_eq!(swept.get(), 1);
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(read(&kept.join(DATABASE)), (vec!["two".to_owned()], 12));
    assert_eq!(
        std::fs::read_to_string(kept.join("settings.json")).unwrap(),
        "{\"a\": 1}"
    );
}

#[test]
fn the_restore_record_reads_back_only_from_a_private_file_and_a_start_forgets_all_but_a_failure() {
    let (root, path) = upgraded(false);
    let record = root.path().join(RESTORE_RECORD_FILE);
    assert_eq!(read_restore_record(root.path()), None);

    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
    forget_restore(&path);
    assert!(!record.exists());

    let failed = format!("{RECORD_FAILED} {RESTORE_BUSY}");
    write_restore_record(root.path(), &failed).unwrap();
    forget_restore(&path);
    assert_eq!(read_restore_record(root.path()), Some(failed));

    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(read_restore_record(root.path()), None);
    std::fs::remove_file(&record).unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::write(&elsewhere, RECORD_PENDING).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &record).unwrap();
    assert_eq!(read_restore_record(root.path()), None);
}
