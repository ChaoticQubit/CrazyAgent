use super::{
    RECORD_FAILED, RECORD_RESTORED, RECORD_RESTORING, REPLACED, RESTORE_BUSY, RESTORE_RECORD_FILE,
    RESTORE_ROLLBACK_FAILED, alone, read_restore_record, real_database, restore_failed,
    write_restore_record,
};
use crate::StorageError;
use crate::migrations::{
    BACKUPS, CHECKED, CopyCheck, DATABASE, V24_VERSION_MAX, check_copy, error, file_digest, schema,
    stamp,
};
use crate::upgrade::{restore, version};
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(test)]
mod tests;

/// The code a recovery action refuses with while the engine, another connection or another
/// recovery action holds engine.db or the restore lock.
pub const PUTBACK_BUSY: &str = "storage_putback_busy";
/// The code a put-back refuses with when the record names no copy, or names one that is gone.
pub const PUTBACK_COPY_MISSING: &str = "storage_putback_copy_missing";
/// The code a put-back refuses with when the named copy fails its check, before any write.
pub const PUTBACK_COPY_FAILED: &str = "storage_putback_copy_failed";
/// The code a recovery action refuses with when engine.db fails the check it needs.
pub const PUTBACK_CHECK_FAILED: &str = "storage_putback_check_failed";
/// The file beside engine.db whose lock one recovery action holds at a time.
pub const RESTORE_LOCK_FILE: &str = "restore.lock";

/// What the recovery screen offers for a restore that did not finish: carry on with the
/// unchanged 2.5 data, finish a restore that wrote the 2.4 data, put the named copy back,
/// or nothing but the backups folder and the code.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recovery {
    Continue,
    Finished { copy: String },
    PutBack { copy: String },
    Stuck { code: String },
}

/// A record that needs the recovery screen: a rollback that failed, or a restore that
/// started and never recorded how it ended, each with the copy it names when the name is a
/// kept copy's.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Broken {
    RollbackFailed(Option<String>),
    Restoring(Option<String>),
}

impl Broken {
    /// The copy the record names. Output: its folder name, or none.
    fn copy(&self) -> Option<&str> {
        match self {
            Broken::RollbackFailed(copy) | Broken::Restoring(copy) => copy.as_deref(),
        }
    }
}

/// Reads the record of the engine at `root` as a broken one, as the engine start refuses
/// it: any line that starts with `restoring` or with `failed storage_restore_rollback_failed`.
/// Anything after that other than one kept copy's name counts as no name. Input: the root.
/// Output: the record, or none for any other.
fn broken_record(root: &Path) -> Option<Broken> {
    let line = read_restore_record(root)?;
    let mut words = line.split(' ');
    let restoring = match words.next()? {
        RECORD_RESTORING => true,
        RECORD_FAILED if words.next() == Some(RESTORE_ROLLBACK_FAILED) => false,
        _ => return None,
    };
    let copy = match (words.next(), words.next()) {
        (Some(word), None) if copy_name(word) => Some(word.to_owned()),
        _ => None,
    };
    Some(match restoring {
        true => Broken::Restoring(copy),
        false => Broken::RollbackFailed(copy),
    })
}

/// Whether a word is a kept copy's folder name: `replaced-` and a time of letters and
/// digits. Input: the word. Output: the answer.
pub(crate) fn copy_name(word: &str) -> bool {
    word.strip_prefix(REPLACED)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|time| !time.is_empty() && time.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Takes the restore lock beside engine.db without waiting: a private file this user owns,
/// opened without following a link. Input: the engine root. Output: the held file, or
/// `storage_putback_busy` while another holds it.
fn locked(root: &Path) -> Result<std::fs::File, StorageError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(root.join(RESTORE_LOCK_FILE))
        .map_err(restore_failed)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| error(PUTBACK_BUSY))?;
    Ok(file)
}

/// Holds the restore lock and engine.db alone for one recovery action. Input: the database
/// path. Output: both, or `storage_putback_busy` while anything else holds either.
fn held(path: &Path) -> Result<(std::fs::File, Connection), StorageError> {
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    let lock = locked(root)?;
    let db = alone(path).map_err(|cause| match cause.0 == RESTORE_BUSY {
        true => error(PUTBACK_BUSY),
        false => cause,
    })?;
    Ok((lock, db))
}

/// Checks engine.db on its held connection: SQLite's integrity check must pass. Input: the
/// connection. Output: its version, or `storage_putback_check_failed`.
fn intact(db: &Connection) -> Result<i64, StorageError> {
    let integrity: String = db
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .map_err(check_failed)?;
    if integrity != "ok" {
        return Err(check_failed(format!(
            "engine.db failed its integrity check: {integrity}"
        )));
    }
    version(db).map_err(|cause| check_failed(cause.0))
}

/// Builds a check refusal. Input: the cause. Output: `storage_putback_check_failed` with it.
fn check_failed(cause: impl std::fmt::Display) -> StorageError {
    error(format!("{PUTBACK_CHECK_FAILED}: {cause}"))
}

/// Builds a copy refusal. Input: the cause. Output: `storage_putback_copy_failed` with it.
fn copy_failed(cause: impl std::fmt::Display) -> StorageError {
    error(format!("{PUTBACK_COPY_FAILED}: {cause}"))
}

/// Checks the named kept copy before anything reads it as 2.5 data: a folder, not a link,
/// under backups/, whose engine.db still matches the digest its `checked` marker holds,
/// passes `check_copy` and reads a 2.5 version. Inputs: the engine root and the copy's name.
/// Output: its folder, version and schema, or the missing or failed refusal.
fn usable(root: &Path, name: &str) -> Result<(PathBuf, i64, Vec<String>), StorageError> {
    assert!(copy_name(name));
    let folder = root.join(BACKUPS).join(name);
    let is_folder = std::fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir());
    if !is_folder {
        return Err(error(PUTBACK_COPY_MISSING));
    }
    let copy = folder.join(DATABASE);
    let marker = std::fs::read(folder.join(CHECKED)).map_err(copy_failed)?;
    let found = file_digest(&copy).map_err(|cause| copy_failed(cause.0))?;
    if marker != found.as_bytes() {
        return Err(copy_failed(
            "the copy does not match the digest it was kept with",
        ));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let opened = Connection::open_with_flags(&copy, flags).map_err(copy_failed)?;
    check_copy(&opened, &copy).map_err(|cause| copy_failed(cause.0))?;
    let found = version(&opened).map_err(|cause| copy_failed(cause.0))?;
    if found <= V24_VERSION_MAX {
        return Err(copy_failed("the copy is not 2.5 data"));
    }
    let rows = schema(&opened).map_err(|cause| copy_failed(cause.0))?;
    Ok((folder, found, rows))
}

/// What the recovery screen offers for the engine at `path`, read under the restore lock with
/// engine.db held alone and written nowhere. A restoring record over data that passes its
/// check offers continue at a 2.5 version and finish at a 2.4 one; a rollback that failed,
/// or a restoring record over data that fails, offers the put-back of a usable named copy.
/// Input: the database path. Output: the offer, none for any other record, or busy.
pub fn recovery(path: &Path) -> Result<Option<Recovery>, StorageError> {
    let path = &real_database(path)?;
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    let (_lock, db) = held(path)?;
    let Some(broken) = broken_record(root) else {
        return Ok(None);
    };
    let live = match broken {
        Broken::Restoring(_) => intact(&db).ok(),
        Broken::RollbackFailed(_) => None,
    };
    let offer = match (live, broken.copy()) {
        (Some(found), _) if found > V24_VERSION_MAX => Recovery::Continue,
        (Some(_), Some(copy)) => Recovery::Finished { copy: copy.into() },
        (_, None) => Recovery::Stuck {
            code: PUTBACK_COPY_MISSING.into(),
        },
        (None, Some(copy)) => match usable(root, copy) {
            Ok(_) => Recovery::PutBack { copy: copy.into() },
            Err(cause) => Recovery::Stuck {
                code: code(&cause).into(),
            },
        },
    };
    Ok(Some(offer))
}

/// The code a refusal starts with. Input: the refusal. Output: its first word.
fn code(cause: &StorageError) -> &str {
    cause.0.split(':').next().unwrap_or(&cause.0)
}

/// Puts the copy the record names back over engine.db, checked as
/// [`put_back_with`] says with `check_copy`. Input: the database path. Output: the folder
/// that keeps the data it replaced.
pub fn put_back(path: &Path) -> Result<PathBuf, StorageError> {
    let path = &real_database(path)?;
    put_back_with(path, check_copy, |_| {})
}

/// Puts the copy back under the restore lock, with engine.db held alone: the record is read
/// under the lock, the copy it names passes its check before any write, engine.db is kept as
/// it is in a new `replaced-<time>` folder, the copy is restored over it on the held
/// connection and checked against it with `check`, and only then is the record removed.
/// Inputs: the database path, the check and a step run on the root before the lock.
/// Output: the folder that keeps the replaced data, or the refusal with the record as it was.
pub(super) fn put_back_with(
    path: &Path,
    check: CopyCheck,
    before_lock: fn(&Path),
) -> Result<PathBuf, StorageError> {
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    before_lock(root);
    let (_lock, mut db) = held(path)?;
    let name = broken_record(root)
        .and_then(|broken| broken.copy().map(str::to_owned))
        .ok_or_else(|| error(PUTBACK_COPY_MISSING))?;
    let (source, found, rows) = usable(root, &name)?;
    let saved = keep_live(path, root, SystemTime::now())?;
    restore(&mut db, &source, found, &rows).map_err(|cause| check_failed(cause.0))?;
    check(&db, &source.join(DATABASE)).map_err(|cause| check_failed(cause.0))?;
    clear(root)?;
    assert!(saved.starts_with(root.join(BACKUPS)));
    Ok(saved)
}

/// Keeps engine.db's bytes as they are in a new 0700 `replaced-<UTC time>` folder before a
/// put-back writes, so it can be undone by hand. The copy is not checked, since the data
/// may fail any check, so it gets no `checked` marker. Inputs: the database path, held
/// alone, the root and the time. Output: the folder.
fn keep_live(path: &Path, root: &Path, now: SystemTime) -> Result<PathBuf, StorageError> {
    let taken = stamp(now).map_err(|cause| restore_failed(cause.0))?;
    let target = root.join(BACKUPS).join(format!("{REPLACED}-{taken}"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(restore_failed)?;
    let copy = target.join(DATABASE);
    let made = std::fs::copy(path, &copy)
        .and_then(|_| std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)))
        .and_then(|()| std::fs::File::open(&copy)?.sync_all())
        .and_then(|()| std::fs::File::open(&target)?.sync_all());
    if let Err(cause) = made {
        let _ = std::fs::remove_dir_all(&target);
        return Err(restore_failed(cause));
    }
    Ok(target)
}

/// Removes the restore record and syncs its folder. Input: the root. Output: Ok once gone.
fn clear(root: &Path) -> Result<(), StorageError> {
    std::fs::remove_file(root.join(RESTORE_RECORD_FILE)).map_err(restore_failed)?;
    std::fs::File::open(root)
        .and_then(|folder| folder.sync_all())
        .map_err(restore_failed)
}

/// Carries on with the 2.5 data after a restore stopped before it changed anything: under the
/// restore lock, engine.db must pass its check again at a 2.5 version before the record is
/// removed. Input: the database path. Output: Ok once the record is gone, or the refusal.
pub fn continue_2_5(path: &Path) -> Result<(), StorageError> {
    let path = &real_database(path)?;
    continue_2_5_with(path, |_| {})
}

/// [`continue_2_5`] with a step run on the root before the lock; the record is read under the
/// lock. Inputs: the database path and the step. Output: as [`continue_2_5`].
pub(super) fn continue_2_5_with(path: &Path, before_lock: fn(&Path)) -> Result<(), StorageError> {
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    before_lock(root);
    let (_lock, db) = held(path)?;
    let Some(Broken::Restoring(_)) = broken_record(root) else {
        return Err(check_failed("the record is not a restore that stopped"));
    };
    let found = intact(&db)?;
    if found <= V24_VERSION_MAX {
        return Err(check_failed("engine.db holds 2.4 data"));
    }
    clear(root)
}

/// Finishes a restore that wrote the 2.4 data and stopped before it recorded so: under the
/// restore lock, engine.db must pass its check at a 2.4 version, the engine's key sweep runs
/// again over the named copy, and the record becomes `restored <copy>`. Inputs: the database
/// path and the sweep. Output: the copy's name, or the refusal with the record as it was.
pub fn finish(
    path: &Path,
    after_restore: &dyn Fn(&Connection, &Path, &Path),
) -> Result<String, StorageError> {
    let path = &real_database(path)?;
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    let (_lock, db) = held(path)?;
    let Some(Broken::Restoring(Some(name))) = broken_record(root) else {
        return Err(error(PUTBACK_COPY_MISSING));
    };
    if intact(&db)? > V24_VERSION_MAX {
        return Err(check_failed("engine.db holds 2.5 data"));
    }
    let folder = root.join(BACKUPS).join(&name);
    if !std::fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir()) {
        return Err(error(PUTBACK_COPY_MISSING));
    }
    after_restore(&db, root, &folder);
    write_restore_record(root, &format!("{RECORD_RESTORED} {name}"))?;
    Ok(name)
}
