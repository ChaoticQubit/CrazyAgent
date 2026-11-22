use crate::StorageError;
use crate::migrations::{
    BACKUPS, DATABASE, PINNED_DAYS, Texts, V24_VERSION_MAX, backup_time, backups_folder,
    backups_in, check_copy, error, fill, mark_checked, pinned, private_file, schema, stamp,
    stamp_time,
};
use crate::upgrade::{restore, version};
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(test)]
mod tests;

/// The parts of a restore the engine owns: the settings texts its kept copy holds, whether an
/// agent still runs (an error refuses the restore as well), and the key sweep that runs on the
/// restored database, held alone, with the engine root and the kept copy's folder.
pub struct Hooks<'a> {
    pub texts: Texts<'a>,
    pub busy: &'a dyn Fn(&Connection) -> Result<bool, StorageError>,
    pub after_restore: &'a dyn Fn(&Connection, &Path, &Path),
}

/// The name a restore gives the dated copy it keeps of the data it replaces (DEC-192), before
/// `-<UTC time>`. It holds no `-to-`, so neither the rotation nor the pin ever counts it.
pub const REPLACED: &str = "replaced";

/// The code a restore starts with once no pinned 2.4 backup is left, after PINNED_DAYS.
pub const RESTORE_UNAVAILABLE: &str = "storage_restore_unavailable";
/// The code a restore starts with while a team agent or a research run still runs.
pub const RESTORE_AGENTS_RUNNING: &str = "storage_restore_agents_running";
/// The code a restore starts with while another connection holds engine.db.
pub const RESTORE_BUSY: &str = "storage_restore_busy";
/// The code any other failed restore starts with.
pub const RESTORE_FAILED: &str = "storage_restore_failed";

/// Until when the owner can still restore the pinned 2.4 backup of the engine at `root`, read
/// without creating anything. Inputs: the engine root and the time now. Output: the moment the
/// pin ends, or None when no pinned backup is left.
pub fn restorable_until(root: &Path, now: SystemTime) -> Option<SystemTime> {
    let folder = root.join(BACKUPS);
    let metadata = std::fs::symlink_metadata(&folder).ok()?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    let found = backups_in(&folder).ok()?;
    let index = pinned(&folder, &found, now)?;
    let until = stamp_time(&found[index].0)? + Duration::from_secs(86_400) * PINNED_DAYS;
    assert!(until > now);
    Some(until)
}

/// Brings back the pinned 2.4 backup the owner asked for (L5, DEC-192), at the time now.
/// Inputs: the database path and the engine's hooks. Output: as [`restore_pinned_at`].
pub fn restore_pinned(path: &Path, hooks: &Hooks<'_>) -> Result<PathBuf, StorageError> {
    restore_pinned_at(path, hooks, SystemTime::now())
}

/// [`restore_pinned`] at a given time, which ages the pin. Inputs: the database path, the
/// engine's hooks and the time. Output: as [`restore_pinned`].
#[cfg(feature = "test-support")]
pub fn restore_pinned_at_for_test(
    path: &Path,
    hooks: &Hooks<'_>,
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    restore_pinned_at(path, hooks, now)
}

/// [`restore_pinned`] at a given time, which ages the pin. It holds engine.db alone for its
/// whole run, refuses while the engine's busy hook says an agent runs or fails, keeps one
/// dated 0700 copy of the data it replaces, then restores the pinned copy, checks every table
/// against it and hands the result to the engine's after-restore hook. Inputs: the database
/// path, the hooks and the time. Output: the folder of the copy it kept.
fn restore_pinned_at(
    path: &Path,
    hooks: &Hooks<'_>,
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    let folder = backups_folder(root)?;
    let found = backups_in(&folder)?;
    let index = pinned(&folder, &found, now).ok_or_else(|| error(RESTORE_UNAVAILABLE))?;
    let source = folder.join(&found[index].1);
    let mut db = alone(path)?;
    if (hooks.busy)(&db)? {
        return Err(error(RESTORE_AGENTS_RUNNING));
    }
    let kept = keep_replaced(&db, &folder, hooks.texts, now)?;
    let pinned_copy = source.join(DATABASE);
    let restored = shape(&pinned_copy).and_then(|(version_pinned, schema_pinned)| {
        restore(&mut db, &source, version_pinned, &schema_pinned)?;
        check_copy(&db, &pinned_copy)?;
        assert_eq!(version(&db)?, version_pinned);
        Ok(())
    });
    if let Err(cause) = restored {
        return Err(drop_unneeded(&db, &kept, cause));
    }
    (hooks.after_restore)(&db, root, &kept);
    assert!(kept.starts_with(&folder));
    Ok(kept)
}

/// Settles the copy a failed restore kept: it is removed when the live database still holds
/// exactly its rows, so retries leave no copies outside the rotation, and kept otherwise,
/// since it may then hold the only whole 2.5 data. Inputs: the connection, the copy folder and
/// the failure. Output: `storage_restore_failed` naming the cause and whether the copy stays.
fn drop_unneeded(db: &Connection, kept: &Path, cause: StorageError) -> StorageError {
    let unchanged = check_copy(db, &kept.join(DATABASE)).is_ok();
    if unchanged && std::fs::remove_dir_all(kept).is_ok() {
        return restore_failed(cause.0);
    }
    restore_failed(format!(
        "{}; the replaced data stays in {}",
        cause.0,
        kept.display()
    ))
}

/// Builds a restore error. Input: the cause. Output: `storage_restore_failed` with it.
pub fn restore_failed(cause: impl std::fmt::Display) -> StorageError {
    error(format!("{RESTORE_FAILED}: {cause}"))
}

/// Opens engine.db and holds it alone until the connection closes: exclusive locking mode
/// and an exclusive transaction, with no wait, as the probe in changes.rs does. Any other
/// connection, in this process or another, refuses the restore, and none can open while it
/// runs. Input: the database path. Output: the connection, or `storage_restore_busy`.
fn alone(path: &Path) -> Result<Connection, StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let db = Connection::open_with_flags(path, flags).map_err(restore_failed)?;
    db.busy_timeout(Duration::ZERO).map_err(restore_failed)?;
    match db.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;") {
        Ok(()) => {}
        Err(rusqlite::Error::SqliteFailure(cause, _))
            if matches!(
                cause.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            return Err(error(RESTORE_BUSY));
        }
        Err(cause) => return Err(restore_failed(cause)),
    }
    assert!(db.is_autocommit());
    let mode: String = db
        .query_row("PRAGMA locking_mode", [], |row| row.get(0))
        .map_err(restore_failed)?;
    assert_eq!(mode, "exclusive");
    Ok(db)
}

/// Keeps one dated copy of the data a restore replaces (DEC-192): engine.db through the
/// SQLite backup API and the settings texts, in a new 0700 `replaced-<UTC time>` folder
/// under backups/, checked and marked as an upgrade backup is. Inputs: the connection, the
/// backups folder, the settings texts and the time. Output: the folder; a copy that fails
/// its check is removed.
fn keep_replaced(
    db: &Connection,
    folder: &Path,
    texts: Texts<'_>,
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    let taken = stamp(now).map_err(|cause| restore_failed(cause.0))?;
    let name = format!("{REPLACED}-{taken}");
    assert!(backup_time(&name).is_none());
    let target = folder.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(restore_failed)?;
    let made = fill(db, &target, texts)
        .and_then(|()| check_copy(db, &target.join(DATABASE)))
        .and_then(|()| mark_checked(folder, &target));
    if let Err(cause) = made {
        let _ = std::fs::remove_dir_all(&target);
        return Err(restore_failed(cause.0));
    }
    Ok(target)
}

/// The version and schema rows of the pinned copy, which the restored database must read.
/// The copy is opened as `check_copy` opens it. Input: the copy's path. Output: both, or
/// `storage_restore_failed` for a copy that is not 2.4 data.
fn shape(copy: &Path) -> Result<(i64, Vec<String>), StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let opened = Connection::open_with_flags(copy, flags).map_err(restore_failed)?;
    let found = version(&opened)?;
    if found > V24_VERSION_MAX {
        return Err(restore_failed("the pinned copy is not 2.4 data"));
    }
    Ok((found, schema(&opened)?))
}

/// The private file beside engine.db that says how the owner's last restore stands, so a
/// client can say so while no engine answers: `pending`, `restored <copy>` or
/// `failed <code>`.
pub const RESTORE_RECORD_FILE: &str = "restore-record";
/// The record of a restore the engine stops for.
pub const RECORD_PENDING: &str = "pending";
/// The first word of the record of a restore that brought the 2.4 data back.
pub const RECORD_RESTORED: &str = "restored";
/// The first word of the record of a restore that changed nothing.
pub const RECORD_FAILED: &str = "failed";
/// The most a restore record holds.
const RESTORE_RECORD_BYTES_MAX: usize = 512;

/// Replaces the restore record of the engine at `root` with one line, written private and
/// synced. Inputs: the root and the line. Output: Ok once it is on disk.
pub fn write_restore_record(root: &Path, line: &str) -> Result<(), StorageError> {
    assert!(!line.is_empty());
    assert!(line.len() <= RESTORE_RECORD_BYTES_MAX);
    let record = root.join(RESTORE_RECORD_FILE);
    match std::fs::remove_file(&record) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(cause) => return Err(restore_failed(cause)),
    }
    private_file(&record, line.as_bytes()).map_err(|cause| restore_failed(cause.0))
}

/// Reads the restore record of the engine at `root`, without an engine and without following
/// a link: only a private regular file of this user counts. Input: the root. Output: its line,
/// or none.
pub fn read_restore_record(root: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(root.join(RESTORE_RECORD_FILE))
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return None;
    }
    let mut text = String::new();
    let limit = u64::try_from(RESTORE_RECORD_BYTES_MAX).ok()?;
    file.take(limit).read_to_string(&mut text).ok()?;
    assert!(text.len() <= RESTORE_RECORD_BYTES_MAX);
    Some(text)
}

/// Removes a pending or restored record as the engine starts, so a stale one never keeps a
/// client from starting the engine after a crash or a return to 2.5; a failed one stays for
/// the restore screen until the next request. Input: the database path. Output: none; a
/// record that cannot be removed is logged.
pub(crate) fn forget_restore(path: &Path) {
    let Some(root) = path.parent() else {
        return;
    };
    let Some(line) = read_restore_record(root) else {
        return;
    };
    if line.split(' ').next() == Some(RECORD_FAILED) {
        return;
    }
    if std::fs::remove_file(root.join(RESTORE_RECORD_FILE)).is_err() {
        eprintln!("storage_restore_record_kept");
    }
}
