use super::*;
use crate::restore::tests::{always_fails, damage, idle, no_texts, read, replaced, upgraded};
use crate::restore::{
    Hooks, RECORD_FAILED, RECORD_RESTORED, RECORD_RESTORING, RESTORE_ROLLBACK_FAILED,
    read_restore_record, refuse_broken, restore_pinned, restore_pinned_at, write_restore_record,
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

/// Runs in a child process: tries to take engine.db out of WAL mode with no busy wait,
/// which needs SQLite's exclusive lock. Input: the database path in
/// `SCRATCH_IDLE_WAL_DB`. Output: exit code 5 when the mode changed, 3 when SQLite was
/// busy, 4 on anything else.
#[test]
#[ignore = "child of scratch_idle_wal_lock_after_a_second_fd_closes"]
fn scratch_idle_wal_child() {
    let Some(path) = std::env::var_os("SCRATCH_IDLE_WAL_DB") else {
        return;
    };
    let db = Connection::open(path).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let code = match db.query_row("PRAGMA journal_mode=DELETE", [], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(mode) if mode == "delete" => 5,
        Ok(_) => 3,
        Err(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            3
        }
        Err(_) => 4,
    };
    std::process::exit(code);
}

/// Prints whether an idle WAL connection still keeps another process from taking the
/// exclusive lock, before and after this process opens and closes a second descriptor of
/// the same file. Input: none. Output: one `IDLE-WAL` line.
#[test]
fn scratch_idle_wal_lock_after_a_second_fd_closes() {
    let root = tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = root.path().join(DATABASE);
    let db = Connection::open(&path).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    db.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    let rows: i64 = db
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    assert!(db.is_autocommit());
    let child = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "restore::recovery::tests::scratch_idle_wal_child",
                "--exact",
                "--ignored",
                "--quiet",
            ])
            .env("SCRATCH_IDLE_WAL_DB", &path)
            .status()
            .unwrap()
            .code()
    };
    let before = child();
    drop(std::fs::File::open(&path).unwrap());
    let after = child();
    println!("IDLE-WAL before={before:?} after={after:?} (5 = the child took the lock, 3 = busy)");
    drop(db);
}

/// Damages engine.db one way. Inputs: the kind (d1 a b-tree page, d2 page 1's cells past
/// the header, d3 the file cut mid-page, d4 the header) and the path. Output: none.
fn scratch_damaged(kind: &str, path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let page = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    assert!(page >= 512 && bytes.len() >= 2 * page);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let at = |file: &mut std::fs::File, offset: usize, data: &[u8]| {
        std::io::Seek::seek(file, std::io::SeekFrom::Start(offset as u64)).unwrap();
        std::io::Write::write_all(file, data).unwrap();
    };
    match kind {
        "d1" => {
            drop(file);
            damage(path);
        }
        "d2" => at(&mut file, page - 64, &[0xaa; 64]),
        "d3" => file.set_len((bytes.len() - page / 2) as u64).unwrap(),
        "d4" => at(&mut file, 0, &[0; 16]),
        other => panic!("no damage {other}"),
    }
}

/// Prints, for each damage, what the backup API makes of engine.db and what today's
/// put-back (a byte copy, then the backup over it) does. Input: none. Output: one
/// `DAMAGE` line per kind.
#[test]
fn scratch_damaged_put_back_routes() {
    for kind in ["d1", "d2", "d3", "d4"] {
        let (root, path, _kept) = broken(true);
        scratch_damaged(kind, &path);
        let live = std::fs::read(&path).unwrap();
        let out = root.path().join("route-a.db");
        let route_a = Connection::open(&path)
            .and_then(|source| {
                let mut target = Connection::open(&out)?;
                rusqlite::backup::Backup::new(&source, &mut target)?.run_to_completion(
                    64,
                    std::time::Duration::ZERO,
                    None,
                )
            })
            .map(|()| std::fs::read(&out).unwrap() == live);
        let put = put_back(&path);
        let kept_equal = put
            .as_ref()
            .ok()
            .map(|saved| std::fs::read(saved.join(DATABASE)).unwrap() == live);
        let put = put.map(|_| ()).map_err(|refused| refused.0);
        println!(
            "DAMAGE {kind} live_bytes={} route_a_backup={route_a:?} put_back_byte_copy={put:?} kept_bytes_equal={kept_equal:?}",
            live.len()
        );
    }
}
