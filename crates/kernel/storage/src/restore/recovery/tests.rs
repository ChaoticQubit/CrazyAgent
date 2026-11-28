use super::*;
use crate::restore::tests::{always_fails, damage, idle, no_texts, read, replaced, upgraded};
use crate::restore::{
    Hooks, RECORD_FAILED, RECORD_RESTORED, RECORD_RESTORING, RESTORE_RECORD_FILE,
    RESTORE_ROLLBACK_FAILED, read_restore_record, refuse_broken, restore_pinned, restore_pinned_at,
    write_restore_record,
};
use std::cell::Cell;
use std::time::SystemTime;

/// Hooks that never refuse and never sweep. Output: the hooks.
fn quiet_hooks() -> Hooks<'static> {
    Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &|_, _, _| {},
    }
}

/// The folder name of a kept copy. Input: its folder. Output: the name.
fn name(folder: &Path) -> String {
    folder.file_name().unwrap().to_str().unwrap().to_owned()
}

/// An engine root after a restore whose rollback failed: the kept copy holds ["two"] at
/// version 12, engine.db holds ["three"] at 12, data the put-back must replace, and the
/// record says the rollback failed, naming the copy or not. Input: whether the record names
/// the copy. Output: the root, the database path and the copy's folder.
fn broken(named: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (root, path) = upgraded(true);
    restore_pinned_at(&path, &quiet_hooks(), SystemTime::now(), always_fails).unwrap_err();
    let kept = replaced(root.path());
    assert_eq!(kept.len(), 1);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DELETE FROM kept; INSERT INTO kept VALUES ('three');")
        .unwrap();
    drop(db);
    let line = match named {
        true => format!(
            "{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED} {}",
            name(&kept[0])
        ),
        false => format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED}"),
    };
    write_restore_record(root.path(), &line).unwrap();
    (root, path, kept[0].clone())
}

/// What a refused put-back must leave as it found: engine.db's bytes, the record and the
/// kept copies. Input: the root and the database path. Output: the three.
fn untouched(root: &Path, path: &Path) -> (Vec<u8>, Option<String>, Vec<PathBuf>) {
    (
        std::fs::read(path).unwrap(),
        read_restore_record(root),
        replaced(root),
    )
}

#[test]
fn a_restore_names_its_kept_copy_in_the_record_before_it_writes_engine_db() {
    let (root, path) = upgraded(true);

    restore_pinned_at(&path, &quiet_hooks(), SystemTime::now(), always_fails).unwrap_err();

    let kept = replaced(root.path());
    assert_eq!(kept.len(), 1);
    let line = format!("{RECORD_RESTORING} {}", name(&kept[0]));
    assert_eq!(read_restore_record(root.path()), Some(line));
}

#[test]
fn a_named_rollback_failed_or_restoring_record_still_refuses_the_start() {
    let (root, path, kept) = broken(true);
    let refused = refuse_broken(&path).unwrap_err().0;
    assert!(
        refused.starts_with(crate::restore::ENGINE_RESTORE_BROKEN),
        "{refused}"
    );

    let line = format!("{RECORD_RESTORING} {}", name(&kept));
    write_restore_record(root.path(), &line).unwrap();
    let refused = refuse_broken(&path).unwrap_err().0;
    assert!(
        refused.starts_with(crate::restore::ENGINE_RESTORE_BROKEN),
        "{refused}"
    );
}

#[test]
fn put_back_restores_the_named_copy_and_clears_the_record() {
    let (root, path, _kept) = broken(true);
    assert_eq!(read(&path), (vec!["three".to_owned()], 12));

    put_back(&path).unwrap();

    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
    assert_eq!(read_restore_record(root.path()), None);
    assert_eq!(refuse_broken(&path), Ok(()));
}

#[test]
fn put_back_first_keeps_engine_db_in_a_new_replaced_copy() {
    let (root, path, kept) = broken(true);

    let saved = put_back(&path).unwrap();

    assert_ne!(saved, kept);
    assert!(saved.starts_with(root.path().join(crate::migrations::BACKUPS)));
    assert!(name(&saved).starts_with(crate::restore::REPLACED));
    let mut found = replaced(root.path());
    found.sort();
    let mut expected = vec![kept, saved.clone()];
    expected.sort();
    assert_eq!(found, expected);
    assert_eq!(read(&saved.join(DATABASE)), (vec!["three".to_owned()], 12));
}

#[test]
fn put_back_refuses_while_another_connection_holds_engine_db_and_writes_nothing() {
    let (root, path, _kept) = broken(true);
    let before = untouched(root.path(), &path);
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();

    let refused = put_back(&path).unwrap_err().0;

    assert!(refused.starts_with(PUTBACK_BUSY), "{refused}");
    holder.execute_batch("ROLLBACK;").unwrap();
    drop(holder);
    assert_eq!(untouched(root.path(), &path), before);
}

#[test]
fn put_back_refuses_while_another_holds_the_restore_lock_and_writes_nothing() {
    let (root, path, _kept) = broken(true);
    let before = untouched(root.path(), &path);
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.path().join(RESTORE_LOCK_FILE))
        .unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();

    let refused = put_back(&path).unwrap_err().0;
    let read_refused = recovery(&path).unwrap_err().0;

    assert!(refused.starts_with(PUTBACK_BUSY), "{refused}");
    assert!(read_refused.starts_with(PUTBACK_BUSY), "{read_refused}");
    drop(held);
    assert_eq!(untouched(root.path(), &path), before);
}

#[test]
fn a_named_copy_that_fails_its_check_is_refused_before_any_write() {
    let (root, path, kept) = broken(true);
    damage(&kept.join(DATABASE));
    let before = untouched(root.path(), &path);
    let copy_before = std::fs::read(kept.join(DATABASE)).unwrap();

    let refused = put_back(&path).unwrap_err().0;

    assert!(refused.starts_with(PUTBACK_COPY_FAILED), "{refused}");
    assert_eq!(untouched(root.path(), &path), before);
    assert_eq!(std::fs::read(kept.join(DATABASE)).unwrap(), copy_before);
    assert_eq!(
        recovery(&path).unwrap(),
        Some(Recovery::Stuck {
            code: PUTBACK_COPY_FAILED.into()
        })
    );
}

#[test]
fn a_record_without_a_name_or_naming_a_missing_copy_offers_no_put_back() {
    let (root, path, _kept) = broken(false);
    let before = untouched(root.path(), &path);
    let stuck = Some(Recovery::Stuck {
        code: PUTBACK_COPY_MISSING.into(),
    });

    assert_eq!(recovery(&path).unwrap(), stuck);
    let refused = put_back(&path).unwrap_err().0;
    assert!(refused.starts_with(PUTBACK_COPY_MISSING), "{refused}");
    assert_eq!(untouched(root.path(), &path), before);

    let gone = format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED} replaced-20990101T000000Z");
    write_restore_record(root.path(), &gone).unwrap();
    let before = untouched(root.path(), &path);
    assert_eq!(recovery(&path).unwrap(), stuck);
    let refused = put_back(&path).unwrap_err().0;
    assert!(refused.starts_with(PUTBACK_COPY_MISSING), "{refused}");
    assert_eq!(untouched(root.path(), &path), before);
}

#[test]
fn a_put_back_whose_result_fails_its_check_keeps_the_record_and_both_copies() {
    let (root, path, kept) = broken(true);
    let line = read_restore_record(root.path());

    let refused = put_back_with(&path, always_fails, |_| {}).unwrap_err().0;

    assert!(refused.starts_with(PUTBACK_CHECK_FAILED), "{refused}");
    assert_eq!(read_restore_record(root.path()), line);
    let found = replaced(root.path());
    assert_eq!(found.len(), 2);
    assert!(found.contains(&kept));
}

#[test]
fn a_rollback_failed_record_with_a_checked_copy_offers_put_back() {
    let (_root, path, kept) = broken(true);

    assert_eq!(
        recovery(&path).unwrap(),
        Some(Recovery::PutBack { copy: name(&kept) })
    );
}

#[test]
fn a_restoring_record_over_unchanged_2_5_data_offers_continue_and_continue_clears_it() {
    let (root, path) = upgraded(true);
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    let before = std::fs::read(&path).unwrap();

    assert_eq!(recovery(&path).unwrap(), Some(Recovery::Continue));
    continue_2_5(&path).unwrap();

    assert_eq!(read_restore_record(root.path()), None);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(refuse_broken(&path), Ok(()));
}

#[test]
fn continue_clears_the_record_only_after_its_own_check_passes() {
    let (root, path) = upgraded(true);
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    assert_eq!(recovery(&path).unwrap(), Some(Recovery::Continue));
    damage(&path);

    let refused = continue_2_5(&path).unwrap_err().0;

    assert!(refused.starts_with(PUTBACK_CHECK_FAILED), "{refused}");
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
}

#[test]
fn a_restoring_record_over_finished_2_4_data_reruns_the_sweep_and_records_restored() {
    let (root, path) = upgraded(true);
    let kept = restore_pinned(&path, &quiet_hooks()).unwrap().kept;
    let line = format!("{RECORD_RESTORING} {}", name(&kept));
    assert_eq!(read_restore_record(root.path()), Some(line));
    assert_eq!(
        recovery(&path).unwrap(),
        Some(Recovery::Finished { copy: name(&kept) })
    );
    let swept = Cell::new(0);

    let copy = finish(&path, &|db, swept_root, folder| {
        assert_eq!(swept_root, root.path());
        assert_eq!(folder, kept);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            11
        );
        swept.set(swept.get() + 1);
    })
    .unwrap();

    assert_eq!(copy, name(&kept));
    assert_eq!(swept.get(), 1);
    let restored = format!("{RECORD_RESTORED} {}", name(&kept));
    assert_eq!(read_restore_record(root.path()), Some(restored));
    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
}

#[test]
fn a_restoring_record_over_damaged_data_offers_put_back_of_its_named_copy_or_nothing() {
    let (root, path) = upgraded(true);
    restore_pinned_at(&path, &quiet_hooks(), SystemTime::now(), always_fails).unwrap_err();
    let kept = replaced(root.path()).remove(0);
    damage(&path);

    assert_eq!(
        recovery(&path).unwrap(),
        Some(Recovery::PutBack { copy: name(&kept) })
    );

    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    assert_eq!(
        recovery(&path).unwrap(),
        Some(Recovery::Stuck {
            code: PUTBACK_COPY_MISSING.into()
        })
    );
}

#[test]
fn any_other_record_needs_no_recovery() {
    let (root, path) = upgraded(true);
    assert_eq!(recovery(&path).unwrap(), None);
    for line in [
        crate::restore::RECORD_PENDING.to_owned(),
        format!("{RECORD_FAILED} {}", crate::restore::RESTORE_BUSY),
        format!("{RECORD_RESTORED} replaced-20260101T000000Z"),
    ] {
        write_restore_record(root.path(), &line).unwrap();
        assert_eq!(recovery(&path).unwrap(), None, "{line}");
        let refused = put_back(&path).unwrap_err().0;
        assert!(
            refused.starts_with(PUTBACK_COPY_MISSING),
            "{line}: {refused}"
        );
    }
}

#[test]
fn a_record_cleared_before_the_put_back_takes_the_lock_is_read_again_under_it() {
    let (root, path, _kept) = broken(true);
    let bytes = std::fs::read(&path).unwrap();
    let copies = replaced(root.path());

    let refused = put_back_with(&path, check_copy, |root| {
        std::fs::remove_file(root.join(RESTORE_RECORD_FILE)).unwrap();
    })
    .unwrap_err()
    .0;

    assert!(refused.starts_with(PUTBACK_COPY_MISSING), "{refused}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(replaced(root.path()), copies);
    assert_eq!(read_restore_record(root.path()), None);
}

#[test]
fn a_record_rewritten_before_continue_takes_the_lock_is_read_again_under_it() {
    let (root, path) = upgraded(true);
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    let line = format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED}");

    let refused = continue_2_5_with(&path, |root| {
        write_restore_record(root, &format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED}")).unwrap();
    })
    .unwrap_err()
    .0;

    assert!(refused.starts_with(PUTBACK_CHECK_FAILED), "{refused}");
    assert_eq!(read_restore_record(root.path()), Some(line));
}

/// A folder that links to an engine root, as a moved home or a data folder on another volume
/// reaches it, kept beside the root in a canonical folder of its own. Input: the root.
/// Output: the folder holding the link, removed on drop, and engine.db's path through it, or
/// the error of the folder or link that failed.
fn linked(root: &Path) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let beside = tempfile::Builder::new().tempdir_in(std::fs::canonicalize("/tmp")?)?;
    let link = beside.path().join("engine");
    std::os::unix::fs::symlink(root, &link)?;
    Ok((beside, link.join(DATABASE)))
}

#[test]
fn an_engine_folder_reached_through_a_link_reads_its_offer_and_restores() -> std::io::Result<()> {
    let (root, real) = upgraded(true);
    let (_beside, path) = linked(root.path())?;

    assert_eq!(recovery(&path).unwrap(), None);
    let restored = restore_pinned(&path, &quiet_hooks());

    assert!(restored.is_ok(), "{:?}", restored.err());
    assert_eq!(read(&real), (vec!["one".to_owned()], 11));
    assert_eq!(replaced(root.path()).len(), 1);
    Ok(())
}

#[test]
fn an_engine_db_that_is_itself_a_link_is_still_refused_before_any_write() {
    let (root, path) = upgraded(true);
    let (other, target) = upgraded(true);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();

    assert!(recovery(&path).is_err());
    assert!(restore_pinned(&path, &quiet_hooks()).is_err());

    assert_eq!(read(&target), (vec!["two".to_owned()], 12));
    assert!(replaced(root.path()).is_empty());
    assert!(replaced(other.path()).is_empty());
}

#[test]
fn a_finish_makes_one_full_flush() {
    let (root, path) = upgraded(true);
    let kept = restore_pinned(&path, &quiet_hooks()).unwrap().kept;
    crate::migrations::take_flushes();

    finish(&path, &|_, _, _| {}).unwrap();

    assert_eq!(crate::migrations::take_flushes().0, 1);
    let restored = format!("{RECORD_RESTORED} {}", name(&kept));
    assert_eq!(read_restore_record(root.path()), Some(restored));
}

#[test]
fn a_put_back_makes_one_full_flush() {
    let (_root, path, _kept) = broken(true);
    crate::migrations::take_flushes();

    put_back(&path).unwrap();

    assert_eq!(crate::migrations::take_flushes().0, 1);
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
}

#[test]
fn put_back_keeps_the_writes_still_in_the_wal_in_its_copy_of_engine_db() {
    let (_root, path, _kept) = broken(true);
    let db = Connection::open(&path).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    db.execute_batch(
        "PRAGMA wal_autocheckpoint = 0; DELETE FROM kept; INSERT INTO kept VALUES ('four');",
    )
    .unwrap();
    drop(db);
    let wal = path.with_file_name(format!("{DATABASE}-wal"));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);

    let saved = put_back(&path).unwrap();

    assert_eq!(read(&saved.join(DATABASE)), (vec!["four".to_owned()], 12));
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
}

#[test]
fn a_restore_a_finish_and_a_put_back_each_end_on_a_fully_flushed_record_write() {
    let (root, path) = upgraded(true);
    write_restore_record(root.path(), crate::restore::RECORD_PENDING).unwrap();
    crate::restore::record::take_writes();
    restore_pinned(&path, &quiet_hooks()).unwrap();
    let pinned = crate::restore::record::take_writes();
    finish(&path, &|_, _, _| {}).unwrap();
    let finished = crate::restore::record::take_writes();
    let (_broken_root, broken_path, _kept) = broken(true);
    crate::restore::record::take_writes();
    put_back(&broken_path).unwrap();
    let put = crate::restore::record::take_writes();

    for (action, writes) in [
        ("restore_pinned", pinned),
        ("finish", finished),
        ("put_back", put),
    ] {
        assert!(!writes.is_empty(), "{action}");
        assert!(writes.iter().all(|flushed| *flushed), "{action} {writes:?}");
    }
}
