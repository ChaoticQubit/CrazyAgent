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

    let kept = restore_pinned(&path, &hooks).unwrap().kept;

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

/// A check after the restore wrote that refuses the pinned copy and checks the kept copy as the
/// restore does, so only the restore's own check fails. Inputs: the connection and the copy.
/// Output: Ok, or `check_forced`.
fn fails_the_pinned_copy(db: &Connection, copy: &Path) -> Result<(), StorageError> {
    let kept = copy
        .parent()
        .and_then(|folder| folder.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(REPLACED));
    if kept {
        return check_copy(db, copy);
    }
    Err(error("check_forced"))
}

/// A check after the restore wrote that refuses every copy, so the rollback fails as well.
/// Inputs: the connection and the copy. Output: `check_forced`.
fn always_fails(_: &Connection, _: &Path) -> Result<(), StorageError> {
    Err(error("check_forced"))
}

/// Runs a restore of the upgraded root whose check after the write is `check`, and returns
/// what it left: the error, the one kept copy and the live data. Input: the check. Output:
/// the root, the database path, the error text and the kept copy's folder.
fn failed_after_write(check: CopyCheck) -> (tempfile::TempDir, PathBuf, String, PathBuf) {
    let (root, path) = upgraded(true);
    let after = Cell::new(0);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &|_, _, _| after.set(after.get() + 1),
    };

    let refused = restore_pinned_at(&path, &hooks, SystemTime::now(), check)
        .unwrap_err()
        .0;

    assert_eq!(after.get(), 0);
    let kept = replaced(root.path());
    assert_eq!(kept.len(), 1, "{refused}");
    assert_eq!(read(&kept[0].join(DATABASE)), (vec!["two".to_owned()], 12));
    let folder = kept[0].clone();
    (root, path, refused, folder)
}

#[test]
fn a_check_that_fails_after_the_restore_wrote_rolls_engine_db_back_and_keeps_the_copy() {
    let (_root, path, refused, kept) = failed_after_write(fails_the_pinned_copy);

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("check_forced"), "{refused}");
    let stays = format!("the replaced data stays in {}", kept.display());
    assert!(refused.ends_with(&stays), "{refused}");
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
}

#[test]
fn a_rollback_that_fails_keeps_the_copy_and_names_both_causes() {
    let (_root, _path, refused, kept) = failed_after_write(always_fails);

    assert!(refused.starts_with(RESTORE_ROLLBACK_FAILED), "{refused}");
    assert_eq!(refused.matches("check_forced").count(), 2, "{refused}");
    let stays = format!("the replaced data stays in {}", kept.display());
    assert!(refused.ends_with(&stays), "{refused}");
}

#[test]
fn a_pinned_copy_the_shape_check_refuses_leaves_no_replaced_copy() {
    let (root, path) = upgraded(true);
    let pinned = std::fs::read_dir(root.path().join(BACKUPS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|folder| folder.join(DATABASE).is_file())
        .unwrap();
    Connection::open(pinned.join(DATABASE))
        .unwrap()
        .pragma_update(None, "user_version", V24_VERSION_MAX + 1)
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    let refused = restore_pinned(
        &path,
        &Hooks {
            texts: &no_texts,
            busy: &idle,
            after_restore: &|_, _, _| panic!("no sweep after a refused restore"),
        },
    )
    .unwrap_err()
    .0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(!refused.contains("stays in"), "{refused}");
    assert!(replaced(root.path()).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A writer that leaves a partial file behind and fails. Inputs: the path and the bytes.
/// Output: `write_forced`.
fn fails_after_a_partial_write(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    std::fs::write(path, &bytes[..1]).unwrap();
    Err(error("write_forced"))
}

#[test]
fn a_record_write_that_fails_keeps_the_old_record_and_leaves_no_temp_file() {
    let root = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let before = std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap();

    let refused = write_record_with(
        root.path(),
        "restored replaced-x",
        fails_after_a_partial_write,
    )
    .unwrap_err()
    .0;

    assert!(refused.contains("write_forced"), "{refused}");
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap(),
        before
    );
    let names = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, [RESTORE_RECORD_FILE]);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
}

/// The folder of the pinned backup [`upgraded`] took. Input: the engine root. Output: it.
fn pinned_folder(root: &Path) -> PathBuf {
    std::fs::read_dir(root.join(BACKUPS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|folder| folder.join(DATABASE).is_file())
        .unwrap()
}

/// A sweep a refused restore must never reach. Inputs: unused. Output: none; it panics.
fn no_sweep(_: &Connection, _: &Path, _: &Path) {
    panic!("no sweep after a refused restore");
}

/// Hooks for a restore the row expects to be refused before its sweep. Output: the hooks.
fn refusing_hooks() -> Hooks<'static> {
    Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &no_sweep,
    }
}

/// H9 (CodeRabbit b01): a start whose restore record exists but cannot be read as a private
/// regular file of this user refuses, since it may say a restore did not finish; only a
/// missing record lets the start go on.
#[test]
fn a_start_whose_restore_record_cannot_be_read_refuses() {
    let (root, path) = upgraded(false);
    let record = root.path().join(RESTORE_RECORD_FILE);
    assert_eq!(refuse_broken(&path), Ok(()));

    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
    let open = refuse_broken(&path).unwrap_err().0;
    assert!(open.starts_with(ENGINE_RESTORE_BROKEN), "{open}");

    std::fs::remove_file(&record).unwrap();
    std::fs::create_dir(&record).unwrap();
    let folder = refuse_broken(&path).unwrap_err().0;
    assert!(folder.starts_with(ENGINE_RESTORE_BROKEN), "{folder}");
}

/// H11 (leader 23:3x): a pin carries the sha256 of its checked engine.db in its `checked`
/// marker, and a copy that still matches it restores as checked by its digest.
#[test]
fn a_pinned_copy_that_matches_its_digest_restores_as_checked() {
    use sha2::{Digest, Sha256};
    let (root, path) = upgraded(true);
    let pinned = pinned_folder(root.path());
    let digest = format!(
        "{:x}",
        Sha256::digest(std::fs::read(pinned.join(DATABASE)).unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(pinned.join(crate::migrations::CHECKED)).unwrap(),
        digest
    );

    let restored = restore_pinned(
        &path,
        &Hooks {
            texts: &no_texts,
            busy: &idle,
            after_restore: &|_, _, _| {},
        },
    )
    .unwrap();

    assert_eq!(restored.pin, PinCheck::Digest);
    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
}

/// H11: a pinned copy whose bytes changed after its backup, here a row with the same
/// version, schema and counts, is refused before any write: engine.db byte-equal and no
/// `replaced-*` copy.
#[test]
fn a_pinned_copy_changed_after_its_backup_is_refused_before_any_write() {
    let (root, path) = upgraded(true);
    Connection::open(pinned_folder(root.path()).join(DATABASE))
        .unwrap()
        .execute("UPDATE kept SET value='foreign'", [])
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("digest"), "{refused}");
    assert!(replaced(root.path()).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// H11 (leader 23:3x): a pin written before digests, with an empty `checked` marker, still
/// restores once `PRAGMA integrity_check` reads ok, and says so as unverified.
#[test]
fn a_pin_with_no_digest_restores_after_its_integrity_check_as_unverified() {
    let (root, path) = upgraded(true);
    std::fs::write(
        pinned_folder(root.path()).join(crate::migrations::CHECKED),
        b"",
    )
    .unwrap();

    let restored = restore_pinned(
        &path,
        &Hooks {
            texts: &no_texts,
            busy: &idle,
            after_restore: &|_, _, _| {},
        },
    )
    .unwrap();

    assert_eq!(restored.pin, PinCheck::Unverified);
    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
    assert_eq!(replaced(root.path()), std::slice::from_ref(&restored.kept));
}

/// H11: a pin with no digest whose integrity check fails (a page no table owns) is refused
/// before any write: engine.db byte-equal and no `replaced-*` copy.
#[test]
fn a_pin_with_no_digest_that_fails_its_integrity_check_is_refused_before_any_write() {
    let (root, path) = upgraded(true);
    let pinned = pinned_folder(root.path());
    std::fs::write(pinned.join(crate::migrations::CHECKED), b"").unwrap();
    let copy = Connection::open(pinned.join(DATABASE)).unwrap();
    copy.execute_batch(
        "CREATE TABLE spare (value TEXT); INSERT INTO spare VALUES ('x');
         PRAGMA writable_schema = ON;
         DELETE FROM sqlite_master WHERE name = 'spare';
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
    drop(copy);
    let before = std::fs::read(&path).unwrap();

    let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("integrity"), "{refused}");
    assert!(replaced(root.path()).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
