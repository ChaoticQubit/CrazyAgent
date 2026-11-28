use super::*;
use crate::migrations::{BACKUPS, DATABASE, backup};
use std::cell::Cell;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

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
pub(super) fn upgraded(pinned: bool) -> (tempfile::TempDir, PathBuf) {
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
pub(super) fn replaced(root: &Path) -> Vec<PathBuf> {
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

/// The env that names the engine.db a [`lock_probe`] child tries to write.
const LOCK_PROBE_ENV: &str = "KERNEL_STORAGE_RESTORE_LOCK_PROBE";
/// The exit code of a [`lock_probe`] child that took the write lock; not 0, which a run that
/// matched no row also gives.
const PROBE_GOT_LOCK: i32 = 5;
/// The exit code of a [`lock_probe`] child refused as busy or locked.
const PROBE_REFUSED: i32 = 3;

/// Run as a child by [`probe_from_another_process`]: with [`LOCK_PROBE_ENV`] set, opens that
/// engine.db with no wait and begins a write, exiting [`PROBE_GOT_LOCK`] when it got the
/// lock, [`PROBE_REFUSED`] when it was busy or locked, and 4 on any other outcome; with the
/// env unset it does nothing, so a plain ignored run passes.
#[test]
#[ignore = "run as a child process by probe_from_another_process"]
fn lock_probe() {
    let Some(path) = std::env::var_os(LOCK_PROBE_ENV) else {
        return;
    };
    let code = match Connection::open(&path).and_then(|db| {
        db.busy_timeout(Duration::ZERO)?;
        db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
    }) {
        Ok(()) => PROBE_GOT_LOCK,
        Err(rusqlite::Error::SqliteFailure(cause, _))
            if matches!(
                cause.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            PROBE_REFUSED
        }
        Err(other) => {
            eprintln!("lock_probe_failed {other}");
            4
        }
    };
    std::process::exit(code);
}

/// Runs [`lock_probe`] in a child process of this test binary against one engine.db and
/// waits for it. Input: the database path. Output: the child's exit code.
fn probe_from_another_process(path: &Path) -> i32 {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "restore::tests::lock_probe",
            "--exact",
            "--ignored",
            "--quiet",
        ])
        .env(LOCK_PROBE_ENV, path)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    status.code().expect("the probe child exits with a code")
}

/// While a restore runs, no other process can write engine.db, in WAL mode as the engine
/// keeps it: a child refused right after the restore holds the database alone is refused
/// again once the restore has copied the replaced data and written the pinned data back.
#[test]
fn a_restore_holds_engine_db_alone_from_its_copy_to_its_end() {
    let (_root, path) = upgraded(true);
    let mode: String = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let held = Cell::new(None);
    let ended = Cell::new(None);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &|_| {
            held.set(Some(probe_from_another_process(&path)));
            Ok(false)
        },
        after_restore: &|_, _, _| ended.set(Some(probe_from_another_process(&path))),
    };

    restore_pinned(&path, &hooks).unwrap();

    assert_eq!(
        held.get(),
        Some(PROBE_REFUSED),
        "the probe is refused once held"
    );
    assert_eq!(
        ended.get(),
        Some(PROBE_REFUSED),
        "another process wrote engine.db while the restore held it"
    );
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
    assert_eq!(read_restore_record(root.path()), None);

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
pub(super) fn always_fails(_: &Connection, _: &Path) -> Result<(), StorageError> {
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

/// A slot writer that writes the slot's first 20 bytes, its header and part of its line,
/// and fails before any flush, as a crash mid-write leaves it. Inputs: the file, the offset and the bytes. Output: an error.
fn tears(file: &std::fs::File, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, &bytes[..20], offset)?;
    Err(std::io::Error::other("write_forced"))
}

#[test]
fn a_record_write_torn_before_its_flush_keeps_the_other_slot_and_the_next_write_goes_on() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();

    let refused = write_record_with(root.path(), "restored replaced-x", tears)
        .unwrap_err()
        .0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("write_forced"), "{refused}");
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
    let mut names = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, [RESTORE_RECORD_FILE, record::RECORD_LOCK]);
    write_restore_record(root.path(), "restored replaced-y").unwrap();
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some("restored replaced-y")
    );
}

/// The folder of the pinned backup [`upgraded`] took. Input: the engine root. Output: it.
pub(super) fn pinned_folder(root: &Path) -> PathBuf {
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

/// A start whose restore record exists but cannot be read as a private
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

/// A pin carries the sha256 of its checked engine.db in its `checked`
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

/// A pinned copy whose bytes changed after its backup, here a row with the same
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

/// A pin written before digests, with an empty `checked` marker, still
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

/// A pin with no digest whose integrity check fails (one table's root page with a page type
/// byte no b-tree has, written into the file, so every SQLite build still opens it) is
/// refused before any write: engine.db byte-equal and no `replaced-*` copy.
#[test]
fn a_pin_with_no_digest_that_fails_its_integrity_check_is_refused_before_any_write() {
    let (root, path) = upgraded(true);
    let pinned = pinned_folder(root.path());
    std::fs::write(pinned.join(crate::migrations::CHECKED), b"").unwrap();
    damage(&pinned.join(DATABASE));
    let before = std::fs::read(&path).unwrap();

    let refused = restore_pinned(&path, &refusing_hooks()).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(refused.contains("integrity"), "{refused}");
    assert!(replaced(root.path()).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
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
fn the_first_record_write_of_a_root_makes_two_full_flushes_and_no_barrier() {
    let (root, _path) = upgraded(false);
    crate::migrations::take_flushes();

    write_restore_record(root.path(), RECORD_PENDING).unwrap();

    assert_eq!(crate::migrations::take_flushes(), (2, 0));
    assert_eq!(
        read_restore_record(root.path()),
        Some(RECORD_PENDING.to_owned())
    );
}

#[test]
fn a_pinned_restore_makes_one_full_flush() {
    let (root, path) = upgraded(true);
    let hooks = Hooks {
        texts: &no_texts,
        busy: &idle,
        after_restore: &|_, _, _| {},
    };
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    crate::migrations::take_flushes();

    let restored = restore_pinned(&path, &hooks).unwrap();

    assert_eq!(crate::migrations::take_flushes().0, 1);
    assert_eq!(restored.pin, PinCheck::Digest);
    assert_eq!(read(&path), (vec!["one".to_owned()], 11));
    assert_eq!(replaced(root.path()), vec![restored.kept]);
}

/// A private engine root in the canonical temp folder. Output: the folder.
fn record_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap()
}

/// Writes `bytes` as the restore record file itself, private to this user. Inputs: the
/// root and the bytes. Output: none.
fn raw_record(root: &Path, bytes: &[u8]) {
    let path = root.join(RESTORE_RECORD_FILE);
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
}

/// Overwrites one whole 4 KiB block of the record file with 0xff. Inputs: the root and
/// the block. Output: none.
fn lose_block(root: &Path, block: u64) {
    use std::os::unix::fs::FileExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(RESTORE_RECORD_FILE))
        .unwrap();
    file.write_all_at(&[0xff; 4096], block * 4096).unwrap();
}

#[test]
fn a_record_write_over_a_record_makes_one_full_flush_and_no_barrier() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    crate::migrations::take_flushes();

    write_restore_record(root.path(), RECORD_RESTORING).unwrap();

    assert_eq!(crate::migrations::take_flushes(), (1, 0));
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
}

#[test]
fn a_whole_block_lost_in_either_slot_leaves_the_other_record_readable() {
    for (block, left) in [(0, "restoring a"), (1, RECORD_PENDING)] {
        let root = record_root();
        write_restore_record(root.path(), RECORD_PENDING).unwrap();
        write_restore_record(root.path(), "restoring a").unwrap();

        lose_block(root.path(), block);
        assert_eq!(read_restore_record(root.path()).as_deref(), Some(left));
        assert_eq!(
            refuse_broken(&root.path().join(DATABASE)).is_err(),
            left != RECORD_PENDING
        );

        write_restore_record(root.path(), "restored b").unwrap();
        assert_eq!(
            read_restore_record(root.path()).as_deref(),
            Some("restored b")
        );
        lose_block(root.path(), block);
        assert_eq!(
            read_restore_record(root.path()).as_deref(),
            Some(left),
            "block {block}"
        );
    }
}

#[test]
fn a_record_of_any_other_size_or_a_short_one_marked_as_slots_is_refused_and_refuses_the_start() {
    let mut cases: Vec<Vec<u8>> = [0, 513, 4096, 8191, 8193, 16384]
        .into_iter()
        .map(|size| vec![b'p'; size])
        .collect();
    let mut marked = b"CARR".to_vec();
    marked.resize(400, b' ');
    cases.push(marked);
    for bytes in cases {
        let root = record_root();
        raw_record(root.path(), &bytes);

        assert_eq!(read_restore_record(root.path()), None, "{}", bytes.len());
        let refused = refuse_broken(&root.path().join(DATABASE)).unwrap_err().0;
        assert!(refused.starts_with(ENGINE_RESTORE_BROKEN), "{refused}");
        assert!(
            write_restore_record(root.path(), RECORD_PENDING).is_err(),
            "{}",
            bytes.len()
        );
        assert_eq!(
            std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap(),
            bytes
        );
    }
}

/// A record left in the one-file format of earlier builds is read as its line on purpose,
/// so a restore that stopped mid-way still refuses the start after an update.
#[test]
fn an_old_one_file_record_is_still_read_and_the_next_write_replaces_it_whole() {
    let root = record_root();
    raw_record(root.path(), b"restoring replaced-x");

    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some("restoring replaced-x")
    );
    assert!(refuse_broken(&root.path().join(DATABASE)).is_err());

    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    assert_eq!(
        std::fs::metadata(root.path().join(RESTORE_RECORD_FILE))
            .unwrap()
            .len(),
        8192
    );
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
    assert!(refuse_broken(&root.path().join(DATABASE)).is_ok());
}

#[test]
fn a_record_write_after_a_seq_wrap_reads_the_wrapped_slot_and_writes_over_the_older() {
    let root = record_root();
    let mut bytes = record::encoded(u64::MAX, "restoring a");
    bytes.extend(record::encoded(0, "restoring b"));
    raw_record(root.path(), &bytes);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some("restoring b")
    );

    write_restore_record(root.path(), RECORD_PENDING).unwrap();

    let after = std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap();
    assert_eq!(
        after[..record::SLOT_BYTES],
        record::encoded(1, RECORD_PENDING)[..]
    );
    assert_eq!(after[record::SLOT_BYTES..], bytes[record::SLOT_BYTES..]);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
}

#[test]
fn a_slot_over_the_line_limit_with_a_matching_digest_is_not_a_record() {
    use sha2::Digest;
    let root = record_root();
    let mut long = b"CARR\x01".to_vec();
    long.extend(9_u64.to_le_bytes());
    long.extend(513_u16.to_le_bytes());
    long.extend([b'r'; 513]);
    let digest = sha2::Sha256::digest(&long);
    long.extend(digest);
    long.resize(record::SLOT_BYTES, 0);
    long.extend(record::encoded(1, RECORD_PENDING));
    raw_record(root.path(), &long);

    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
}

/// Both slots torn leave a record that cannot be read: the start is refused with the same
/// `engine_restore_broken` text as any unreadable record, and a write is refused, never a
/// repair that would hide a torn `restoring`.
#[test]
fn a_record_with_both_slots_torn_refuses_the_start_and_every_write() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    write_restore_record(root.path(), "restoring replaced-x").unwrap();
    lose_block(root.path(), 0);
    lose_block(root.path(), 1);
    let torn = std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap();

    assert_eq!(read_restore_record(root.path()), None);
    let refused = refuse_broken(&root.path().join(DATABASE)).unwrap_err().0;
    assert_eq!(
        refused,
        format!(
            "{ENGINE_RESTORE_BROKEN}: the engine data did not pass its check after a failed \
             restore; the replaced 2.5 data stays in the {BACKUPS} folder"
        )
    );
    let written = write_restore_record(root.path(), RECORD_PENDING)
        .unwrap_err()
        .0;
    assert_eq!(written, format!("{RESTORE_FAILED}: {}", record::UNREADABLE));
    forget_restore(&root.path().join(DATABASE));
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_RECORD_FILE)).unwrap(),
        torn
    );
}

#[test]
fn a_start_forgets_a_pending_record_with_no_flush_and_keeps_a_restoring_one() {
    let root = record_root();
    let path = root.path().join(DATABASE);
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    crate::migrations::take_flushes();

    forget_restore(&path);

    assert_eq!(crate::migrations::take_flushes(), (0, 0));
    assert_eq!(read_restore_record(root.path()), None);
    assert!(refuse_broken(&path).is_ok());
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    forget_restore(&path);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
}

#[test]
fn the_record_lock_is_close_on_exec_and_free_again_while_a_copy_of_it_lives() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let lock = record::locked(root.path()).unwrap();
    let flags = rustix::io::fcntl_getfd(&lock.0).unwrap();
    assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    let copy = lock.0.try_clone().unwrap();

    drop(lock);

    let started = std::time::Instant::now();
    write_restore_record(root.path(), RECORD_RESTORING).unwrap();
    assert!(started.elapsed() < record::LOCK_WAIT);
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_RESTORING)
    );
    drop(copy);
}

#[test]
fn a_record_lock_held_for_good_fails_the_write_naming_the_lock_within_its_wait() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let holder = std::fs::File::open(root.path().join(record::RECORD_LOCK)).unwrap();
    rustix::fs::flock(&holder, rustix::fs::FlockOperation::LockExclusive).unwrap();

    let started = std::time::Instant::now();
    let refused = write_restore_record(root.path(), RECORD_RESTORING)
        .unwrap_err()
        .0;
    let waited = started.elapsed();

    assert_eq!(refused, format!("{RESTORE_FAILED}: {}", record::LOCK_HELD));
    assert!(waited >= record::LOCK_WAIT, "{waited:?}");
    assert!(
        waited < record::LOCK_WAIT + std::time::Duration::from_millis(1500),
        "{waited:?}"
    );
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
}

#[test]
fn a_record_lock_that_is_a_link_or_not_private_refuses_the_write() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let lock = root.path().join(record::RECORD_LOCK);
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let open = write_restore_record(root.path(), RECORD_RESTORING)
        .unwrap_err()
        .0;
    assert!(open.starts_with(RESTORE_FAILED), "{open}");

    std::fs::remove_file(&lock).unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::write(&elsewhere, b"").unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &lock).unwrap();
    let linked = write_restore_record(root.path(), RECORD_RESTORING)
        .unwrap_err()
        .0;
    assert!(linked.starts_with(RESTORE_FAILED), "{linked}");
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
}

#[test]
fn two_threads_writing_one_record_never_leave_it_unreadable() {
    let root = record_root();
    write_restore_record(root.path(), RECORD_PENDING).unwrap();
    let lines: Vec<String> = (0..2)
        .flat_map(|writer| (0..40).map(move |n| format!("restoring w{writer}-{n}")))
        .chain([RECORD_PENDING.to_owned()])
        .collect();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..2)
            .map(|writer| {
                let root = root.path();
                scope.spawn(move || {
                    for n in 0..40 {
                        write_restore_record(root, &format!("restoring w{writer}-{n}")).unwrap();
                    }
                })
            })
            .collect();
        let reader = scope.spawn(|| {
            let mut reads = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let line = read_restore_record(root.path()).expect("readable");
                assert!(lines.contains(&line), "{line}");
                reads += 1;
            }
            reads
        });
        for writer in writers {
            writer.join().unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(reader.join().unwrap() > 0);
    });
    let last = read_restore_record(root.path()).unwrap();
    assert!(
        last == "restoring w0-39" || last == "restoring w1-39",
        "{last}"
    );
}

#[test]
fn a_flushed_write_after_a_failed_folder_flush_flushes_the_folder_before_it_returns_ok() {
    let root = record_root();
    record::fail_next_folder_flush();
    let failed = write_restore_record(root.path(), RECORD_PENDING)
        .unwrap_err()
        .0;
    assert!(failed.contains("folder flush failed"), "{failed}");
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some(RECORD_PENDING)
    );
    crate::migrations::take_flushes();

    write_restore_record(root.path(), RECORD_RESTORING).unwrap();

    assert_eq!(crate::migrations::take_flushes(), (2, 0));
    write_restore_record(root.path(), "restored replaced-x").unwrap();
    assert_eq!(crate::migrations::take_flushes(), (1, 0));
    assert_eq!(
        read_restore_record(root.path()).as_deref(),
        Some("restored replaced-x")
    );
}
