use super::*;
use crate::migrations::{BACKUPS, DATABASE, backup};
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

/// A fixture step's result with the step named in its error. Inputs: the result and the
/// step. Output: the value, or the step and the cause.
pub(super) fn named<T, E: std::fmt::Display>(
    result: Result<T, E>,
    step: &str,
) -> Result<T, String> {
    result.map_err(|cause| format!("{step}: {cause}"))
}

/// A fresh folder in the configured temp folder, through its real path, so every path built
/// in it is canonical. Output: the folder, removed when dropped.
pub(super) fn temp_root() -> Result<tempfile::TempDir, String> {
    let base = named(
        std::fs::canonicalize(std::env::temp_dir()),
        "find the temp folder",
    )?;
    named(
        tempfile::Builder::new().tempdir_in(&base),
        &format!("make a folder in {}", base.display()),
    )
}

/// The bytes of a fixture file. Input: its path. Output: them, or the path and the cause.
fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    named(std::fs::read(path), &format!("read {}", path.display()))
}

/// The names in a fixture folder. Input: its path. Output: them, or the path and the cause.
fn names(folder: &Path) -> Result<Vec<String>, String> {
    let listed = named(
        std::fs::read_dir(folder),
        &format!("list {}", folder.display()),
    )?;
    let mut found = Vec::new();
    for entry in listed {
        let entry = named(entry, &format!("list {}", folder.display()))?;
        found.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(found)
}

/// No settings texts. Input: none. Output: an empty list.
pub(super) fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// No agent runs. Input: the connection. Output: false.
pub(super) fn idle(_: &Connection) -> Result<bool, StorageError> {
    Ok(false)
}

/// An engine root in a canonical private folder whose engine.db was upgraded from 2.4: a
/// checked `11-to-12` backup holds `kept` = ["one"] at version 11, and engine.db now holds
/// ["two"] at version 12. Input: whether to take the pinned backup. Output: the root and
/// the database path.
pub(super) fn upgraded(pinned: bool) -> Result<(tempfile::TempDir, PathBuf), String> {
    let root = temp_root()?;
    let path = root.path().join(DATABASE);
    let db = named(Connection::open(&path), "open engine.db")?;
    named(
        db.execute_batch(
            "CREATE TABLE kept (value TEXT); INSERT INTO kept VALUES ('one'); \
             PRAGMA user_version = 11;",
        ),
        "write the 2.4 data",
    )?;
    if pinned {
        named(
            backup(&db, &path, &no_texts, (11, 12), SystemTime::now()),
            "take the pinned backup",
        )?;
    }
    named(
        db.execute_batch(
            "DELETE FROM kept; INSERT INTO kept VALUES ('two'); PRAGMA user_version = 12;",
        ),
        "write the 2.5 data",
    )?;
    drop(db);
    Ok((root, path))
}

/// The values in `kept` and the version of a database. Input: its path. Output: both.
pub(super) fn read(path: &Path) -> (Vec<String>, i64) {
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
pub(super) fn replaced(root: &Path) -> Result<Vec<PathBuf>, String> {
    let folder = root.join(BACKUPS);
    Ok(names(&folder)?
        .into_iter()
        .filter(|name| name.starts_with("replaced-"))
        .map(|name| folder.join(name))
        .collect())
}

/// Runs a restore whose busy hook answers `busy` and checks it is refused with `code`
/// before any copy: engine.db byte-equal, no replaced copy and no after-restore call.
/// Inputs: the hook's answer and the code. Output: none.
fn refused_before_any_copy(busy: Result<bool, StorageError>, code: &str) -> Result<(), String> {
    let (root, path) = upgraded(true)?;
    let before = bytes(&path)?;
    let after = Cell::new(0);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &|_| busy.clone(),
        after_restore: &|_, _, _| after.set(after.get() + 1),
    };

    let refused = restore_pinned(&path, &hooks).unwrap_err().0;

    assert_eq!(refused, code);
    assert_eq!(bytes(&path)?, before);
    assert!(replaced(root.path())?.is_empty());
    assert_eq!(after.get(), 0);
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
    Ok(())
}

#[test]
fn a_restore_while_an_agent_runs_is_refused_before_any_copy() -> Result<(), String> {
    refused_before_any_copy(Ok(true), RESTORE_AGENTS_RUNNING)
}

#[test]
fn a_busy_hook_that_fails_refuses_the_restore_before_any_copy() -> Result<(), String> {
    refused_before_any_copy(Err(StorageError("probe_failed".to_owned())), "probe_failed")
}

#[test]
fn a_restore_while_another_connection_holds_the_database_is_refused_as_busy() -> Result<(), String>
{
    let (root, path) = upgraded(true)?;
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
    assert!(replaced(root.path())?.is_empty());
    holder.execute_batch("ROLLBACK;").unwrap();
    Ok(())
}

#[test]
fn with_no_pinned_backup_the_restore_is_unavailable_and_asks_nothing() -> Result<(), String> {
    let (root, path) = upgraded(false)?;
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
    Ok(())
}

#[test]
fn a_restore_brings_the_2_4_data_back_and_keeps_the_replaced_data() -> Result<(), String> {
    let (root, path) = upgraded(true)?;
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
    assert_eq!(replaced(root.path())?, std::slice::from_ref(&kept));
    assert_eq!(swept.get(), 1);
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(read(&kept.join(DATABASE)), (vec!["two".to_owned()], 12));
    assert_eq!(
        named(
            std::fs::read_to_string(kept.join("settings.json")),
            "read the kept settings.json",
        )?,
        "{\"a\": 1}"
    );
    Ok(())
}

#[test]
fn the_restore_record_reads_back_only_from_a_private_file_and_a_start_forgets_all_but_a_failure()
-> Result<(), String> {
    let (root, path) = upgraded(false)?;
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
    Ok(())
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
pub(super) fn always_fails(_: &Connection, _: &Path) -> Result<(), StorageError> {
    Err(error("check_forced"))
}

/// Runs a restore of the upgraded root whose check after the write is `check`, and returns
/// what it left: the error, the one kept copy and the live data. Input: the check. Output:
/// the root, the database path, the error text and the kept copy's folder.
fn failed_after_write(
    check: CopyCheck,
) -> Result<(tempfile::TempDir, PathBuf, String, PathBuf), String> {
    let (root, path) = upgraded(true)?;
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
    let kept = replaced(root.path())?;
    assert_eq!(kept.len(), 1, "{refused}");
    assert_eq!(read(&kept[0].join(DATABASE)), (vec!["two".to_owned()], 12));
    let folder = kept[0].clone();
    Ok((root, path, refused, folder))
}

#[test]
fn a_check_that_fails_after_the_restore_wrote_rolls_engine_db_back_and_keeps_the_copy()
-> Result<(), String> {
    let (_root, path, refused, kept) = failed_after_write(fails_the_pinned_copy)?;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("check_forced"), "{refused}");
    let stays = format!("the replaced data stays in {}", kept.display());
    assert!(refused.ends_with(&stays), "{refused}");
    assert_eq!(read(&path), (vec!["two".to_owned()], 12));
    Ok(())
}

#[test]
fn a_rollback_that_fails_keeps_the_copy_and_names_both_causes() -> Result<(), String> {
    let (_root, _path, refused, kept) = failed_after_write(always_fails)?;

    assert!(refused.starts_with(RESTORE_ROLLBACK_FAILED), "{refused}");
    assert_eq!(refused.matches("check_forced").count(), 2, "{refused}");
    let stays = format!("the replaced data stays in {}", kept.display());
    assert!(refused.ends_with(&stays), "{refused}");
    Ok(())
}

#[test]
fn a_pinned_copy_the_shape_check_refuses_leaves_no_replaced_copy() -> Result<(), String> {
    let (root, path) = upgraded(true)?;
    let pinned = pinned_folder(root.path())?;
    Connection::open(pinned.join(DATABASE))
        .unwrap()
        .pragma_update(None, "user_version", V24_VERSION_MAX + 1)
        .unwrap();
    let before = bytes(&path)?;

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
    assert!(replaced(root.path())?.is_empty());
    assert_eq!(bytes(&path)?, before);
    Ok(())
}

/// A writer that leaves a partial file behind and fails. Inputs: the path and the bytes.
/// Output: `write_forced`.
fn fails_after_a_partial_write(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    std::fs::write(path, &bytes[..1]).unwrap();
    Err(error("write_forced"))
}

#[test]
fn a_record_write_that_fails_keeps_the_old_record_and_leaves_no_temp_file() -> Result<(), String> {
    let root = temp_root()?;
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let before = bytes(&root.path().join(RESTORE_RECORD_FILE))?;

    let refused = write_record_with(
        root.path(),
        "restored replaced-x",
        fails_after_a_partial_write,
    )
    .unwrap_err()
    .0;

    assert!(refused.contains("write_forced"), "{refused}");
    assert_eq!(bytes(&root.path().join(RESTORE_RECORD_FILE))?, before);
    assert_eq!(names(root.path())?, [RESTORE_RECORD_FILE]);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
    Ok(())
}

#[test]
fn a_record_line_that_is_empty_too_long_or_not_one_line_is_refused_and_keeps_the_old_record()
-> Result<(), String> {
    let root = temp_root()?;
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let long = "x".repeat(RESTORE_RECORD_BYTES_MAX + 1);
    for line in [
        "",
        "failed x\nrestored y",
        "failed x\rrestored y",
        long.as_str(),
    ] {
        let refused = write_restore_record(root.path(), line).unwrap_err().0;
        assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
        assert_eq!(
            read_restore_record(root.path()).as_deref(),
            Some(RECORD_PENDING)
        );
    }
    assert_eq!(names(root.path())?, [RESTORE_RECORD_FILE]);
    Ok(())
}

#[test]
fn a_record_longer_than_one_record_is_refused_and_never_read_cut_short() -> Result<(), String> {
    let root = temp_root()?;
    let file = root.path().join(RESTORE_RECORD_FILE);
    std::fs::write(
        &file,
        format!("{RECORD_PENDING}{}", " x".repeat(RESTORE_RECORD_BYTES_MAX)),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(read_restore_record(root.path()), None);
    assert!(refuse_broken(&root.path().join(DATABASE)).is_err());
    Ok(())
}

/// The folder of the pinned backup [`upgraded`] took. Input: the engine root. Output: it.
pub(super) fn pinned_folder(root: &Path) -> Result<PathBuf, String> {
    let folder = root.join(BACKUPS);
    names(&folder)?
        .into_iter()
        .map(|name| folder.join(name))
        .find(|backup| backup.join(DATABASE).is_file())
        .ok_or_else(|| format!("no pinned backup in {}", folder.display()))
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

/// A start whose restore record exists but cannot be read as a private
/// regular file of this user refuses, since it may say a restore did not finish; only a
/// missing record lets the start go on.
#[test]
fn a_start_whose_restore_record_cannot_be_read_refuses() -> Result<(), String> {
    let (root, path) = upgraded(false)?;
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
    Ok(())
}

/// A pin carries the sha256 of its checked engine.db in its `checked`
/// marker, and a copy that still matches it restores as checked by its digest.
#[test]
fn a_pinned_copy_that_matches_its_digest_restores_as_checked() -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let (root, path) = upgraded(true)?;
    let pinned = pinned_folder(root.path())?;
    let digest = format!("{:x}", Sha256::digest(bytes(&pinned.join(DATABASE))?));
    assert_eq!(
        named(
            std::fs::read_to_string(pinned.join(crate::migrations::CHECKED)),
            "read the pin's checked marker",
        )?,
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
    Ok(())
}

/// A pinned copy whose bytes changed after its backup, here a row with the same
/// version, schema and counts, is refused before any write: engine.db byte-equal and no
/// `replaced-*` copy.
#[test]
fn a_pinned_copy_changed_after_its_backup_is_refused_before_any_write() -> Result<(), String> {
    let (root, path) = upgraded(true)?;
    Connection::open(pinned_folder(root.path())?.join(DATABASE))
        .unwrap()
        .execute("UPDATE kept SET value='foreign'", [])
        .unwrap();
    let before = bytes(&path)?;

    let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("digest"), "{refused}");
    assert!(replaced(root.path())?.is_empty());
    assert_eq!(bytes(&path)?, before);
    Ok(())
}

#[test]
fn a_pinned_copy_whose_digest_marker_is_not_a_digest_is_refused_before_any_write()
-> Result<(), String> {
    for length in [DIGEST_HEX_BYTES + 1, 4 << 20] {
        let (root, path) = upgraded(true)?;
        std::fs::write(
            pinned_folder(root.path())?.join(crate::migrations::CHECKED),
            "a".repeat(length),
        )
        .unwrap();
        let before = bytes(&path)?;

        let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

        assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
        assert!(refused.contains("digest marker"), "{refused}");
        assert!(replaced(root.path())?.is_empty());
        assert_eq!(bytes(&path)?, before);
    }
    Ok(())
}

/// A pin written before digests, with an empty `checked` marker, still
/// restores once `PRAGMA integrity_check` reads ok, and says so as unverified.
#[test]
fn a_pin_with_no_digest_restores_after_its_integrity_check_as_unverified() -> Result<(), String> {
    let (root, path) = upgraded(true)?;
    std::fs::write(
        pinned_folder(root.path())?.join(crate::migrations::CHECKED),
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
    assert_eq!(replaced(root.path())?, std::slice::from_ref(&restored.kept));
    Ok(())
}

/// A pin with no digest whose integrity check fails (one table's root page with a page type
/// byte no b-tree has, written into the file, so every SQLite build still opens it) is
/// refused before any write: engine.db byte-equal and no `replaced-*` copy.
#[test]
fn a_pin_with_no_digest_that_fails_its_integrity_check_is_refused_before_any_write()
-> Result<(), String> {
    let (root, path) = upgraded(true)?;
    let pinned = pinned_folder(root.path())?;
    std::fs::write(pinned.join(crate::migrations::CHECKED), b"").unwrap();
    damage(&pinned.join(DATABASE));
    let before = bytes(&path)?;

    let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("integrity"), "{refused}");
    assert!(replaced(root.path())?.is_empty());
    assert_eq!(bytes(&path)?, before);
    Ok(())
}

/// Damages a database so SQLite still opens it but its integrity check fails: a new table
/// `spare` gets one row, and the first byte of its root page, the page type, is overwritten.
/// The schema on page 1 stays readable. Input: the database path. Output: none.
pub(super) fn damage(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE spare (value TEXT); INSERT INTO spare VALUES ('x');")
        .unwrap();
    let journal: String = db
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "delete", "every page is in the file");
    let (root_page, page_size): (i64, i64) = db
        .query_row(
            "SELECT rootpage, (SELECT page_size FROM pragma_page_size)
            FROM sqlite_master WHERE name = 'spare'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(db);
    assert!(root_page > 1, "page 1 keeps the schema readable");
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    std::io::Seek::seek(
        &mut file,
        std::io::SeekFrom::Start(((root_page - 1) * page_size) as u64),
    )
    .unwrap();
    std::io::Write::write_all(&mut file, &[0xff]).unwrap();
}

#[test]
fn a_bare_database_name_is_the_database_in_the_working_folder() {
    assert_eq!(
        real_database(Path::new(DATABASE)).unwrap(),
        std::fs::canonicalize(".").unwrap().join(DATABASE)
    );
}

#[test]
fn a_restore_record_write_makes_one_full_flush() -> Result<(), String> {
    let (root, _path) = upgraded(false)?;
    crate::migrations::take_flushes();

    write_restore_record(root.path(), RECORD_PENDING).unwrap();

    assert_eq!(crate::migrations::take_flushes().0, 1);
    assert_eq!(
        read_restore_record(root.path()),
        Some(RECORD_PENDING.to_owned())
    );
    Ok(())
}

#[test]
fn a_pinned_restore_makes_one_full_flush() -> Result<(), String> {
    let (root, path) = upgraded(true)?;
    let hooks = Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &|_, _, _| {},
    };
    crate::migrations::take_flushes();

    let restored = restore_pinned(&path, &hooks).unwrap();

    assert_eq!(crate::migrations::take_flushes().0, 1);
    assert_eq!(restored.pin, PinCheck::Digest);
    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
    assert_eq!(replaced(root.path())?, vec![restored.kept]);
    Ok(())
}
