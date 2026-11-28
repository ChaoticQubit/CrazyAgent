use std::hint::black_box;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use super::{nanos, sampled};

use rusqlite::{Connection, OpenFlags, Transaction};

use super::row;
use crate::StorageError;
use crate::migrations::{BACKUPS, DATABASE, backup, check_copy};
use crate::perf_kit::{SAMPLES, time};
use crate::restore::recovery::{Recovery, continue_2_5, finish, put_back, recovery};
use crate::restore::{
    Hooks, RECORD_FAILED, RECORD_RESTORING, RESTORE_ROLLBACK_FAILED, read_restore_record,
    restore_pinned, write_restore_record,
};
use crate::upgrade::{Step, check, upgrade};

/// The sized engine.db both builds time: 4,096 rows of 256 bytes, about 1.1 MiB. The same
/// bytes are in the 2.4.4 twin file.
const FIXTURE_SQL: &str = "CREATE TABLE fixture (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4096)
INSERT INTO fixture SELECT i, printf('%0256d', i) FROM n;";
const FIXTURE_ROWS: i64 = 4096;
const NEWEST: i64 = 12;
const PINNED_FROM: i64 = 11;
const HEAVY_SAMPLES: usize = 40;
const READ_BATCH: usize = 10;

/// No settings texts: the 2.4.4 twin passes no settings sources either. Output: an empty list.
fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// No agent runs. Input: the connection. Output: false.
fn idle(_: &Connection) -> Result<bool, StorageError> {
    Ok(false)
}

/// A step that changes nothing, so the upgrade rows time the upgrade itself. Input: the
/// transaction. Output: Ok.
fn no_change(_: &Transaction<'_>) -> Result<(), StorageError> {
    Ok(())
}

/// Steps 2 to [`NEWEST`], each changing nothing. Output: the steps.
fn steps() -> Vec<Step> {
    (2..=NEWEST).map(|to| Step { to, run: no_change }).collect()
}

/// Hooks that never refuse and never sweep. Output: the hooks.
fn quiet_hooks() -> Hooks<'static> {
    Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &|_, _, _| {},
    }
}

/// An engine root in a canonical private temp folder holding the sized engine.db at a
/// version. Input: the version. Output: the folder guard, the root and the database path.
fn sized(at: i64) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").expect("tmp"))
        .expect("temp");
    let root = temp.path().join("engine");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("root");
    let path = root.join(DATABASE);
    let db = Connection::open(&path).expect("db");
    db.execute_batch(FIXTURE_SQL).expect("fixture");
    db.pragma_update(None, "user_version", at).expect("version");
    drop(db);
    (temp, root, path)
}

/// The sized engine.db upgraded from 2.4: a checked `11-to-12` backup holds it at version
/// 11, and engine.db is at version 12. Output: the folder guard, the root and the path.
fn upgraded() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (temp, root, path) = sized(PINNED_FROM);
    let db = Connection::open(&path).expect("db");
    backup(
        &db,
        &path,
        &no_texts,
        (PINNED_FROM, NEWEST),
        SystemTime::now(),
    )
    .expect("backup");
    db.pragma_update(None, "user_version", NEWEST)
        .expect("version");
    drop(db);
    (temp, root, path)
}

/// Copies a folder tree, keeping each file's mode and making each folder 0700. Inputs: the
/// source and the target, which must not exist. Output: none; a failure panics.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(to)
        .expect("folder");
    for entry in std::fs::read_dir(from).expect("list") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

/// Removes every backup in a backups folder and keeps the folder, as an engine root holds it
/// between upgrades, outside the timed span. Input: the folder. Output: none.
fn clear_backups(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries {
        std::fs::remove_dir_all(entry.expect("entry").path()).expect("remove");
    }
}

/// Puts an engine root back to a template's state outside the timed span: removes it and
/// copies the template over. Inputs: the template and the root. Output: none.
fn reset(template: &Path, root: &Path) {
    std::fs::remove_dir_all(root).expect("remove");
    copy_tree(template, root);
}

/// The kept copy a restore names in its record. Input: the root. Output: its name.
fn named_copy(root: &Path) -> String {
    let line = read_restore_record(root).expect("record");
    line.split(' ').nth(1).expect("copy").to_owned()
}

/// Times backup, upgrade, restore and recovery over one sized engine.db of 4,096 rows of
/// 256 bytes, about 1.1 MiB. Rows that change the root reset it from a template before each
/// call outside the timed span. No settings texts are kept on either build. Prints one PERF
/// line per call.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn storage_sized_database_calls() {
    let steps = steps();

    let (_backup_temp, backup_root, backup_path) = sized(NEWEST);
    let backup_db = Connection::open(&backup_path).expect("db");
    let copy = backup(
        &backup_db,
        &backup_path,
        &no_texts,
        (NEWEST, NEWEST + 1),
        SystemTime::now(),
    )
    .expect("backup");
    check_copy(&backup_db, &copy.join(DATABASE)).expect("copy check");
    let rows: i64 = backup_db
        .query_row("SELECT COUNT(*) FROM fixture", [], |row| row.get(0))
        .expect("rows");
    assert_eq!(rows, FIXTURE_ROWS);
    sampled(&row("migrations.backup", 1), HEAVY_SAMPLES, || {
        clear_backups(&backup_root.join(BACKUPS));
        let started = Instant::now();
        black_box(
            backup(
                &backup_db,
                &backup_path,
                &no_texts,
                (NEWEST, NEWEST + 1),
                SystemTime::now(),
            )
            .expect("backup"),
        );
        nanos(started)
    });
    let copy = backup(
        &backup_db,
        &backup_path,
        &no_texts,
        (NEWEST, NEWEST + 1),
        SystemTime::now(),
    )
    .expect("backup");
    time(&row("migrations.check_copy", 1), HEAVY_SAMPLES, || {
        check_copy(black_box(&backup_db), black_box(&copy.join(DATABASE))).expect("copy check");
    });
    drop(backup_db);

    let read_only = Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("read only");
    check(&read_only, &steps).expect("check");
    time(&row("upgrade.check", READ_BATCH), SAMPLES, || {
        check(black_box(&read_only), black_box(&steps)).expect("check");
    });
    drop(read_only);

    let (_upgrade_temp, upgrade_root, upgrade_path) = sized(NEWEST);
    let mut upgrade_db = Connection::open(&upgrade_path).expect("db");
    time(&row("upgrade.upgrade.newest", READ_BATCH), SAMPLES, || {
        upgrade(&mut upgrade_db, &upgrade_path, &no_texts, &steps).expect("newest");
    });
    sampled(&row("upgrade.upgrade.one_step", 1), HEAVY_SAMPLES, || {
        upgrade_db
            .pragma_update(None, "user_version", NEWEST - 1)
            .expect("version");
        clear_backups(&upgrade_root.join(BACKUPS));
        let started = Instant::now();
        upgrade(&mut upgrade_db, &upgrade_path, &no_texts, &steps).expect("one step");
        nanos(started)
    });
    let found: i64 = upgrade_db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(found, NEWEST);
    drop(upgrade_db);

    let (_restore_temp, template, template_path) = upgraded();
    let root = template.with_file_name("sample");
    copy_tree(&template, &root);
    let path = root.join(DATABASE);
    let hooks = quiet_hooks();
    restore_pinned(&path, &hooks).expect("restore");
    let kept = named_copy(&root);
    assert_eq!(
        recovery(&path).expect("recovery"),
        Some(Recovery::Finished { copy: kept.clone() })
    );
    sampled(&row("restore.restore_pinned", 1), HEAVY_SAMPLES, || {
        reset(&template, &root);
        let started = Instant::now();
        black_box(restore_pinned(&path, &hooks).expect("restore"));
        nanos(started)
    });

    reset(&template, &root);
    restore_pinned(&path, &hooks).expect("restore");
    let finished = template.with_file_name("finished");
    let _ = std::fs::remove_dir_all(&finished);
    copy_tree(&root, &finished);
    let kept = named_copy(&finished);
    time(&row("restore.recovery.recovery", 1), HEAVY_SAMPLES, || {
        black_box(recovery(black_box(&path)).expect("recovery"));
    });
    let restoring = format!("{RECORD_RESTORING} {kept}");
    sampled(&row("restore.recovery.finish", 1), HEAVY_SAMPLES, || {
        write_restore_record(&root, &restoring).expect("record");
        let started = Instant::now();
        black_box(finish(&path, &|_, _, _| {}).expect("finish"));
        nanos(started)
    });

    let failed = format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED} {kept}");
    reset(&finished, &root);
    write_restore_record(&root, &failed).expect("record");
    let broken = template.with_file_name("broken");
    copy_tree(&root, &broken);
    assert_eq!(
        recovery(&path).expect("recovery"),
        Some(Recovery::PutBack { copy: kept.clone() })
    );
    sampled(&row("restore.recovery.put_back", 1), HEAVY_SAMPLES, || {
        reset(&broken, &root);
        let started = Instant::now();
        black_box(put_back(&path).expect("put back"));
        nanos(started)
    });

    reset(&template, &root);
    write_restore_record(&root, &restoring).expect("record");
    assert_eq!(recovery(&path).expect("recovery"), Some(Recovery::Continue));
    sampled(
        &row("restore.recovery.continue_2_5", 1),
        HEAVY_SAMPLES,
        || {
            write_restore_record(&root, &restoring).expect("record");
            let started = Instant::now();
            continue_2_5(&path).expect("continue");
            nanos(started)
        },
    );
    assert_eq!(read_restore_record(&root), None);
    assert!(template_path.is_file());
}
