use crate::StorageError;
use crate::migrations::{
    BACKUPS, CHECKED, CopyCheck, DATABASE, DIGEST_HEX_BYTES, PINNED_DAYS, Texts, V24_VERSION_MAX,
    backup_time, backups_folder, backups_in, check_copy, error, file_digest, fill, mark_checked,
    pinned, private_file, schema, stamp, stamp_time,
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

/// How a restore knew the pinned copy was the one its upgrade checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinCheck {
    /// The copy's sha256 matched the digest its `checked` marker holds.
    Digest,
    /// The marker holds no digest, as a 2.4 build writes it, so the copy passed
    /// `PRAGMA integrity_check` and the schema and row checks alone.
    Unverified,
}

/// A restore that brought the pinned 2.4 data back: the folder of the copy kept of the data
/// it replaced, and how the pinned copy was checked before any write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    pub kept: PathBuf,
    pub pin: PinCheck,
}

/// The code a restore starts with once no pinned 2.4 backup is left, after PINNED_DAYS.
pub const RESTORE_UNAVAILABLE: &str = "storage_restore_unavailable";
/// The code a restore starts with while a team agent or a research run still runs.
pub const RESTORE_AGENTS_RUNNING: &str = "storage_restore_agents_running";
/// The code a restore starts with while another connection holds engine.db.
pub const RESTORE_BUSY: &str = "storage_restore_busy";
/// The code any other failed restore starts with.
pub const RESTORE_FAILED: &str = "storage_restore_failed";
/// The code a restore starts with when it wrote engine.db, failed, and could not bring the
/// replaced data back either, so engine.db holds neither copy for sure.
pub const RESTORE_ROLLBACK_FAILED: &str = "storage_restore_rollback_failed";
/// The code an engine start refuses with while the restore record says a rollback failed,
/// so data that did not pass its check after a restore is never migrated.
pub const ENGINE_RESTORE_BROKEN: &str = "engine_restore_broken";

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
pub fn restore_pinned(path: &Path, hooks: &Hooks<'_>) -> Result<Restored, StorageError> {
    restore_pinned_at(path, hooks, SystemTime::now(), check_copy)
}

/// [`restore_pinned`] at a given time, which ages the pin. Inputs: the database path, the
/// engine's hooks and the time. Output: as [`restore_pinned`].
#[cfg(feature = "test-support")]
pub fn restore_pinned_at_for_test(
    path: &Path,
    hooks: &Hooks<'_>,
    now: SystemTime,
) -> Result<Restored, StorageError> {
    restore_pinned_at(path, hooks, now, check_copy)
}

/// [`restore_pinned`] at a given time, which ages the pin. It holds engine.db alone for its
/// whole run, refuses while the engine's busy hook says an agent runs or fails, keeps one
/// dated 0700 copy of the data it replaces, then restores the pinned copy, checks every table
/// against it and hands the result to the engine's after-restore hook. Inputs: the database
/// path, the hooks, the time and the check of the live database against a copy once the
/// restore wrote it. The pinned copy is checked against its digest, or without one by its
/// integrity check, before any write. Output: the kept copy's folder and that pin check.
fn restore_pinned_at(
    path: &Path,
    hooks: &Hooks<'_>,
    now: SystemTime,
    check: CopyCheck,
) -> Result<Restored, StorageError> {
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
    let pinned_copy = source.join(DATABASE);
    let pin = pin_check(&source)?;
    let (version_pinned, schema_pinned) =
        shape(&pinned_copy, pin).map_err(|cause| restore_failed(cause.0))?;
    let (version_live, schema_live) = (version(&db)?, schema(&db)?);
    let kept = keep_replaced(&db, &folder, hooks.texts, now)?;
    let restored = restore(&mut db, &source, version_pinned, &schema_pinned).and_then(|()| {
        check(&db, &pinned_copy)?;
        assert_eq!(version(&db)?, version_pinned);
        Ok(())
    });
    if let Err(cause) = restored {
        return Err(roll_back(
            &mut db,
            &kept,
            (version_live, &schema_live),
            check,
            cause,
        ));
    }
    (hooks.after_restore)(&db, root, &kept);
    assert!(kept.starts_with(&folder));
    Ok(Restored { kept, pin })
}

/// Checks a pinned backup against the digest its `checked` marker holds:
/// a match passes as [`PinCheck::Digest`], an empty marker, as a 2.4 build writes it, passes
/// as [`PinCheck::Unverified`] for [`shape`] to run the integrity check, and anything else is
/// refused. Input: the backup folder. Output: the check, or `storage_restore_failed`.
fn pin_check(source: &Path) -> Result<PinCheck, StorageError> {
    let marker = std::fs::read(source.join(CHECKED)).map_err(restore_failed)?;
    if marker.is_empty() {
        return Ok(PinCheck::Unverified);
    }
    let found = file_digest(&source.join(DATABASE)).map_err(|cause| restore_failed(cause.0))?;
    assert_eq!(found.len(), DIGEST_HEX_BYTES);
    if marker != found.as_bytes() {
        return Err(restore_failed(
            "the pinned copy does not match the digest its backup recorded",
        ));
    }
    Ok(PinCheck::Digest)
}

/// Brings the replaced data back into engine.db from the copy kept of it, after a restore
/// that wrote engine.db failed, on the same held connection, and checks it against that copy.
/// The copy always stays. Inputs: the connection, the copy's folder, the version and schema
/// it holds, the copy check and the failure. Output: `storage_restore_failed` naming the cause
/// and the copy, or `storage_restore_rollback_failed` naming both causes and the copy.
fn roll_back(
    db: &mut Connection,
    kept: &Path,
    (found, before): (i64, &[String]),
    check: CopyCheck,
    cause: StorageError,
) -> StorageError {
    let back = restore(db, kept, found, before).and_then(|()| check(db, &kept.join(DATABASE)));
    let stays = format!("the replaced data stays in {}", kept.display());
    match back {
        Ok(()) => restore_failed(format!("{}; {stays}", cause.0)),
        Err(rollback) => error(format!(
            "{RESTORE_ROLLBACK_FAILED}: {}; the rollback failed: {}; {stays}",
            cause.0, rollback.0
        )),
    }
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
/// The copy is opened as `check_copy` opens it; one with no digest must also pass
/// `PRAGMA integrity_check`. Inputs: the copy's path and its pin check. Output: both, or
/// `storage_restore_failed` for a copy that is not 2.4 data or fails that check.
fn shape(copy: &Path, pin: PinCheck) -> Result<(i64, Vec<String>), StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let opened = Connection::open_with_flags(copy, flags).map_err(restore_failed)?;
    if pin == PinCheck::Unverified {
        let integrity: String = opened
            .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
            .map_err(restore_failed)?;
        if integrity != "ok" {
            return Err(restore_failed(
                "the pinned copy with no digest failed its integrity check",
            ));
        }
    }
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
/// The record a restore writes before it starts, which its outcome replaces; left behind by a
/// crash or a failed outcome write, it refuses the next start.
pub const RECORD_RESTORING: &str = "restoring";
/// The word a restored record ends with when the pinned copy had no digest to check.
pub const RECORD_UNVERIFIED_PIN: &str = "unverified_pin";
/// The first word of the record of a restore that brought the 2.4 data back.
pub const RECORD_RESTORED: &str = "restored";
/// The first word of the record of a restore that did not finish.
pub const RECORD_FAILED: &str = "failed";
/// The most a restore record holds.
const RESTORE_RECORD_BYTES_MAX: usize = 512;

/// Replaces the restore record of the engine at `root` with one line: the line goes to a private
/// temp file beside it, synced, which is then renamed over the record, and the folder is
/// synced, so a failure at any step leaves the old record whole. Inputs: the root and the
/// line. Output: Ok once it is on disk.
pub fn write_restore_record(root: &Path, line: &str) -> Result<(), StorageError> {
    write_record_with(root, line, private_file)
}

/// The temp file a record write fills before it renames it over the record.
const RESTORE_RECORD_TEMP: &str = "restore-record.new";

/// [`write_restore_record`] with the private file writer given, so a row can fail it. A temp
/// file left by an earlier crash is removed first, and the temp file is removed after any
/// failure before the rename. Inputs: the root, the line and the writer. Output: as
/// [`write_restore_record`].
fn write_record_with(
    root: &Path,
    line: &str,
    write: fn(&Path, &[u8]) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    assert!(!line.is_empty());
    assert!(line.len() <= RESTORE_RECORD_BYTES_MAX);
    let record = root.join(RESTORE_RECORD_FILE);
    let temp = root.join(RESTORE_RECORD_TEMP);
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(cause) => return Err(restore_failed(cause)),
    }
    let renamed = write(&temp, line.as_bytes())
        .and_then(|()| std::fs::rename(&temp, &record).map_err(restore_failed));
    if let Err(cause) = renamed {
        let _ = std::fs::remove_file(&temp);
        return Err(restore_failed(cause.0));
    }
    std::fs::File::open(root)
        .and_then(|folder| folder.sync_all())
        .map_err(restore_failed)
}

/// Refuses an engine start while the restore record says a restore wrote engine.db and could
/// not roll it back, or started and never recorded how it ended, or cannot be read, so data
/// that did not pass its check is never migrated. Input: the database path. Output: Ok, or
/// `engine_restore_broken`.
pub(crate) fn refuse_broken(path: &Path) -> Result<(), StorageError> {
    let Some(root) = path.parent() else {
        return Ok(());
    };
    let broken = format!("{RECORD_FAILED} {RESTORE_ROLLBACK_FAILED}");
    let refused = match read_record_strict(root) {
        Ok(None) => false,
        Ok(Some(line)) => line == broken || line == RECORD_RESTORING,
        Err(()) => true,
    };
    if refused {
        return Err(error(format!(
            "{ENGINE_RESTORE_BROKEN}: the engine data did not pass its check after a failed \
             restore; the replaced 2.5 data stays in the {BACKUPS} folder"
        )));
    }
    Ok(())
}

/// Reads the restore record of the engine at `root`, without an engine and without following
/// a link: only a private regular file of this user counts. Input: the root. Output: its line,
/// or none, for a missing record and for one that cannot be read alike.
pub fn read_restore_record(root: &Path) -> Option<String> {
    read_record_strict(root).ok().flatten()
}

/// [`read_restore_record`] that tells a missing record from one that exists but cannot be
/// read as a private regular file of this user. Input: the root. Output: Ok(None) when
/// it is missing, Ok(Some) with its line, or Err for any other failure.
fn read_record_strict(root: &Path) -> Result<Option<String>, ()> {
    use std::io::Read;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(root.join(RESTORE_RECORD_FILE));
    let file = match opened {
        Ok(file) => file,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(());
    }
    let mut text = String::new();
    let limit = u64::try_from(RESTORE_RECORD_BYTES_MAX).map_err(|_| ())?;
    file.take(limit).read_to_string(&mut text).map_err(|_| ())?;
    assert!(text.len() <= RESTORE_RECORD_BYTES_MAX);
    Ok(Some(text))
}

/// Removes a pending or restored record as the engine starts, so a stale one never keeps a
/// client from starting the engine after a crash or a return to 2.5; a failed or restoring
/// one stays for the restore screen until the next request. Input: the database path. Output: none; a
/// record that cannot be removed is logged.
pub(crate) fn forget_restore(path: &Path) {
    let Some(root) = path.parent() else {
        return;
    };
    let Some(line) = read_restore_record(root) else {
        return;
    };
    if line == RECORD_RESTORING || line.split(' ').next() == Some(RECORD_FAILED) {
        return;
    }
    if std::fs::remove_file(root.join(RESTORE_RECORD_FILE)).is_err() {
        eprintln!("storage_restore_record_kept");
    }
}
