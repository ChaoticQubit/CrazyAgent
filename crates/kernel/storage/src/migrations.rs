use crate::StorageError;
#[cfg(windows)]
use crazyagent_win_sandbox::private;
use rusqlite::{Connection, OpenFlags};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(all(test, unix))]
mod tests;

/// The settings texts an upgrade backup keeps beside engine.db, read once after the
/// database copy: each a plain file name and its bytes. The engine reads them from its
/// settings sources; a database with none passes a list that is empty.
pub type Texts<'a> = &'a dyn Fn() -> Result<Vec<(&'static str, String)>, StorageError>;

/// The Windows open flag that lets a folder be opened as a handle.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// The Windows open flag that opens a link as itself, the twin of Unix `O_NOFOLLOW` on the
/// last path component.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Builds a storage error from any cause, with its text unchanged. Input: the cause.
/// Output: the error.
pub(crate) fn error(value: impl std::fmt::Display) -> StorageError {
    StorageError(value.to_string())
}

/// The folder beside engine.db that holds one folder per upgrade backup, each with a full
/// engine.db copy and the settings texts kept with it, and the dated copies a restore keeps.
/// The sandbox names it among the engine secrets, so no granted root may
/// hold or sit inside it.
pub const BACKUPS: &str = "backups";

/// How many upgrade backups of this user stay on disk: the new one and the newest others.
/// Older ones are removed once a new one exists; another account's folders are never touched.
pub const BACKUPS_KEPT: usize = 5;

const _: () = assert!(BACKUPS_KEPT >= 1);

/// The newest engine.db version a 2.4 engine writes: 2.4.2 ships the steps up to 11. An
/// upgrade from at most this version to a newer one is a 2.5 upgrade of 2.4 data.
pub const V24_VERSION_MAX: i64 = 11;

/// How many days the first backup a 2.5 upgrade took from a 2.4 database stays outside the
/// BACKUPS_KEPT rotation, so the owner can still go back to 2.4 in that time.
pub const PINNED_DAYS: u32 = 30;

/// The file a backup folder holds once its copy passed the check made before any step ran.
pub const CHECKED: &str = "checked";

/// The most tables the copy check counts. engine.db holds a few hundred.
const TABLES_MAX: usize = 4_096;

/// Checks a fresh backup copy against the live database before any step runs. Inputs: the
/// live connection and the copy's path. Output: Ok, or the refusal that stops the upgrade.
pub type CopyCheck = fn(&Connection, &Path) -> Result<(), StorageError>;

/// The database file name inside a backup folder.
pub const DATABASE: &str = "engine.db";

/// The schema rows of engine.db, sorted, used to see whether a failed upgrade left
/// the database changed. Input: the connection. Output: one line per schema row.
pub(crate) fn schema(db: &Connection) -> Result<Vec<String>, StorageError> {
    let mut statement = db
        .prepare(
            "SELECT type || ' ' || name || ' ' || tbl_name || ' ' || coalesce(sql, '') \
             FROM sqlite_schema ORDER BY type, name",
        )
        .map_err(error)?;
    statement
        .query_map([], |row| row.get(0))
        .map_err(error)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(error)
}

/// Builds a backup error. Input: the cause. Output: `storage_backup_failed` with it.
fn backup_failed(cause: impl std::fmt::Display) -> StorageError {
    error(format!("{BACKUP_FAILED}: {cause}"))
}

/// Copies engine.db through the SQLite online backup API, and the settings files, into
/// `<root>/backups/<from>-to-<to>-<UTC time>/` (folders 0700, files 0600; on Windows a
/// database store, so the copies take engine.db's full medium label), checks the copy,
/// marks it checked, then keeps it, the pinned backup and the newest others of this user
/// ([`prune`]).
/// Inputs: the connection, its path, the settings texts, both versions and the time.
/// Output: the new backup folder; a copy that fails the real check is removed.
pub fn backup(
    db: &Connection,
    path: &Path,
    texts: Texts<'_>,
    versions: (i64, i64),
    now: SystemTime,
) -> Result<PathBuf, StorageError> {
    backup_checked(db, path, texts, versions, now, check_copy)
}

/// [`backup`] with another copy check, so a row can stand in a check that refuses.
/// Inputs: as [`backup`], plus the check. Output: as [`backup`].
#[cfg(feature = "test-support")]
pub fn backup_for_test(
    db: &Connection,
    path: &Path,
    texts: Texts<'_>,
    versions: (i64, i64),
    now: SystemTime,
    check: CopyCheck,
) -> Result<PathBuf, StorageError> {
    backup_checked(db, path, texts, versions, now, check)
}

/// [`backup`] with its copy check named. Inputs: as [`backup_for_test`]. Output: as
/// [`backup`].
fn backup_checked(
    db: &Connection,
    path: &Path,
    texts: Texts<'_>,
    (from, to): (i64, i64),
    now: SystemTime,
    check: CopyCheck,
) -> Result<PathBuf, StorageError> {
    assert!(from < to);
    let root = path
        .parent()
        .ok_or_else(|| backup_failed("the database has no folder"))?;
    let folder = backups_folder(root)?;
    let target = folder.join(format!("{from}-to-{to}-{}", stamp(now)?));
    #[cfg(unix)]
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(backup_failed)?;
    #[cfg(windows)]
    {
        std::fs::create_dir(&target).map_err(backup_failed)?;
        if let Err(cause) = private::store_dir(&target, private::Store::Database) {
            let _ = std::fs::remove_dir(&target);
            return Err(backup_failed(cause));
        }
    }
    let made = checked_copy(db, &folder, &target, texts, check, Flush::Full);
    if let Err(cause) = made {
        let _ = std::fs::remove_dir_all(&target);
        return Err(cause);
    }
    prune(&folder, &target, now)?;
    Ok(target)
}

/// Checks a backup copy before any step runs: SQLite's integrity check must pass, the copy
/// must hold engine.db's schema, and every table must hold as many rows as in engine.db.
/// The copy is opened without create, by the same path `fill` wrote it to, which may pass a
/// linked engine root. Inputs: the live connection and the copy's path.
/// Output: Ok, or `storage_backup_failed` naming the gap.
pub fn check_copy(db: &Connection, copy: &Path) -> Result<(), StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let opened = Connection::open_with_flags(copy, flags).map_err(backup_failed)?;
    let integrity: String = opened
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .map_err(backup_failed)?;
    if integrity != "ok" {
        return Err(backup_failed(format!(
            "the backup copy failed its integrity check: {integrity}"
        )));
    }
    let schemas = schema(&opened).and_then(|copied| Ok((copied, schema(db)?)));
    let (copied, live) = schemas.map_err(|cause| backup_failed(cause.0))?;
    if copied != live {
        return Err(backup_failed("the backup copy holds another schema"));
    }
    for table in tables(db)? {
        let live = count(db, &table)?;
        let copied = count(&opened, &table)?;
        if copied != live {
            return Err(backup_failed(format!(
                "the backup copy holds {copied} rows in {table} where engine.db holds {live}"
            )));
        }
    }
    Ok(())
}

/// The tables of a database, by name. Input: the connection. Output: at most TABLES_MAX
/// names, or `storage_backup_failed` when it holds more.
fn tables(db: &Connection) -> Result<Vec<String>, StorageError> {
    let mut statement = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .map_err(backup_failed)?;
    let names = statement
        .query_map([], |row| row.get(0))
        .map_err(backup_failed)?
        .take(TABLES_MAX + 1)
        .collect::<Result<Vec<String>, _>>()
        .map_err(backup_failed)?;
    if names.len() > TABLES_MAX {
        return Err(backup_failed(format!(
            "engine.db holds more than {TABLES_MAX} tables"
        )));
    }
    Ok(names)
}

/// The rows one table holds. Inputs: the connection and a table name from its schema.
/// Output: the count.
fn count(db: &Connection, table: &str) -> Result<i64, StorageError> {
    let quoted = table.replace('"', "\"\"");
    let rows: i64 = db
        .query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
            row.get(0)
        })
        .map_err(backup_failed)?;
    assert!(rows >= 0);
    Ok(rows)
}

/// Fills a new backup folder, checks its copy and marks it checked: the copy's sha256 is
/// read on a second thread while `check` reads the copy against the live database, and both
/// must pass. Inputs: the connection, the backups folder, the new backup, the settings
/// texts, the check and the flush of the backups folder. Output: Ok, or the first failure,
/// after which the caller removes the folder.
pub(crate) fn checked_copy(
    db: &Connection,
    folder: &Path,
    target: &Path,
    texts: Texts<'_>,
    check: CopyCheck,
    last: Flush,
) -> Result<(), StorageError> {
    fill(db, target, texts)?;
    let copy = target.join(DATABASE);
    let (checked, digest) = std::thread::scope(|scope| {
        let hashing = scope.spawn(|| file_digest(&copy));
        let checked = check(db, &copy);
        let digest = hashing
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (checked, digest)
    });
    checked?;
    mark_checked(folder, target, &digest?, last)
}

/// Reads the `checked` digest marker of a backup or a kept copy without following a link,
/// at most one byte past a digest, so a grown file is never read whole. Input: the folder.
/// Output: the marker, empty as a 2.4 build writes it or the 64 hex digest, or the cause
/// for a marker that cannot be read or is neither.
#[cfg(unix)]
pub(crate) fn digest_marker(folder: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(folder.join(CHECKED))
        .map_err(|cause| format!("the digest marker cannot be read: {cause}"))?;
    let mut marker = Vec::with_capacity(DIGEST_HEX_BYTES + 1);
    let limit = u64::try_from(DIGEST_HEX_BYTES + 1).map_err(|cause| cause.to_string())?;
    file.take(limit)
        .read_to_end(&mut marker)
        .map_err(|cause| format!("the digest marker cannot be read: {cause}"))?;
    assert!(marker.len() <= DIGEST_HEX_BYTES + 1);
    if !marker.is_empty() && marker.len() != DIGEST_HEX_BYTES {
        return Err("the digest marker is not a digest".to_owned());
    }
    Ok(marker)
}

/// Records that a backup copy passed its check and makes the record durable: the `checked`
/// file, holding the sha256 of the checked engine.db so a restore can tell the copy has not
/// changed since, and the backup folder, each behind a barrier, then the backups folder
/// flushed as `last` says, so a crash after a full flush still finds the backup checked; on
/// Windows each folder is flushed through W1's `sync_folder`, as a plain open refuses a
/// folder there. Inputs: the backups folder, the new backup, the copy's digest and the last
/// flush. Output: Ok once flushed.
pub(crate) fn mark_checked(
    folder: &Path,
    target: &Path,
    digest: &str,
    last: Flush,
) -> Result<(), StorageError> {
    assert!(target.starts_with(folder));
    assert_eq!(digest.len(), DIGEST_HEX_BYTES);
    private_file(&target.join(CHECKED), digest.as_bytes(), Flush::Barrier)?;
    for (synced, kind) in [(target, Flush::Barrier), (folder, last)] {
        #[cfg(unix)]
        std::fs::File::open(synced)
            .and_then(|handle| flush(&handle, kind))
            .map_err(backup_failed)?;
        #[cfg(windows)]
        {
            let _ = kind;
            private::sync_folder(synced).map_err(backup_failed)?;
        }
    }
    Ok(())
}

/// How many hex characters a sha256 digest in a `checked` marker holds.
pub(crate) const DIGEST_HEX_BYTES: usize = 64;

/// The sha256 of a file's bytes, read in a stream so a large engine.db is never held in
/// memory. Input: the path. Output: the lowercase hex digest, or the read's failure.
pub(crate) fn file_digest(path: &Path) -> Result<String, StorageError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(backup_failed)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(backup_failed)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Creates `<root>/backups` when missing, or makes a folder of this user there private
/// again when an older build or a copy left it open to others (F36), and checks it. A link,
/// a file or a folder of another user is refused. Input: the engine root. Output: the
/// backups folder path.
#[cfg(unix)]
pub(crate) fn backups_folder(root: &Path) -> Result<PathBuf, StorageError> {
    let folder = root.join(BACKUPS);
    match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(cause) => return Err(backup_failed(cause)),
    }
    let unsafe_folder = || {
        error(format!(
            "{BACKUP_UNSAFE}: {} must be a private folder of this user",
            folder.display()
        ))
    };
    let metadata = std::fs::symlink_metadata(&folder).map_err(backup_failed)?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(unsafe_folder());
    }
    if metadata.mode() & 0o077 != 0 {
        make_private(&folder, &metadata).map_err(|_| unsafe_folder())?;
        eprintln!("storage_backup_folder_made_private");
    }
    let checked = std::fs::symlink_metadata(&folder).map_err(backup_failed)?;
    if !checked.is_dir()
        || checked.uid() != rustix::process::getuid().as_raw()
        || checked.mode() & 0o077 != 0
        || (checked.dev(), checked.ino()) != (metadata.dev(), metadata.ino())
    {
        return Err(unsafe_folder());
    }
    Ok(folder)
}

/// Creates `<root>/backups` as a W1 private folder when missing, or opens and checks the one
/// there, and makes a local folder of this user there private again when an older build or
/// a copy left it open to others (F36, DESIGN-0256 D5). A link, a file or a folder of another
/// user is refused as unsafe. Input: the engine root. Output: the backups folder path.
#[cfg(windows)]
pub(crate) fn backups_folder(root: &Path) -> Result<PathBuf, StorageError> {
    let folder = root.join(BACKUPS);
    match private::dir(&folder) {
        Ok(_) => Ok(folder),
        Err(cause) if cause.kind() == std::io::ErrorKind::PermissionDenied => {
            match private::repair(&folder) {
                Ok(_) => {
                    eprintln!("storage_backup_folder_made_private");
                    Ok(folder)
                }
                Err(_) => Err(error(format!(
                    "{BACKUP_UNSAFE}: {} must be a private folder of this user",
                    folder.display()
                ))),
            }
        }
        Err(cause) => Err(backup_failed(cause)),
    }
}

/// Sets one folder of this user to 0700 through a handle opened without following a link,
/// only when it is still the folder that was checked. Inputs: the folder and its checked
/// metadata. Output: done, or the refusal or I/O error.
#[cfg(unix)]
fn make_private(folder: &Path, checked: &std::fs::Metadata) -> Result<(), StorageError> {
    use rustix::fs::{Mode, OFlags};
    let handle = std::fs::File::from(
        rustix::fs::open(
            folder,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?,
    );
    let opened = handle.metadata().map_err(error)?;
    if !opened.is_dir()
        || (opened.dev(), opened.ino()) != (checked.dev(), checked.ino())
        || opened.uid() != rustix::process::getuid().as_raw()
    {
        return Err(error(BACKUP_UNSAFE));
    }
    rustix::fs::fchmod(&handle, Mode::RWXU).map_err(error)
}

/// The code a backup refused for an unsafe backups folder starts with.
const BACKUP_UNSAFE: &str = "storage_backup_unsafe";

/// The code any other failed backup starts with.
pub const BACKUP_FAILED: &str = "storage_backup_failed";

/// The private file beside engine.db that names why the last upgrade backup failed, so a
/// client can say so while no engine answers (F36). A successful backup removes it.
pub const BACKUP_FAILURE_FILE: &str = "backup-failed";

/// Keeps a durable record of how an upgrade backup ended: a failure writes its code into
/// [`BACKUP_FAILURE_FILE`], a success removes the file. Writing the record never changes
/// the result. Inputs: the database path and the backup's result. Output: the same result.
pub(crate) fn recorded(
    path: &Path,
    result: Result<PathBuf, StorageError>,
) -> Result<PathBuf, StorageError> {
    let Some(root) = path.parent() else {
        return result;
    };
    let record = root.join(BACKUP_FAILURE_FILE);
    match &result {
        Ok(_) => forget_backup_failure(path),
        Err(cause) => {
            let code = if cause.0.starts_with(BACKUP_UNSAFE) {
                BACKUP_UNSAFE
            } else {
                BACKUP_FAILED
            };
            let _ = std::fs::remove_file(&record);
            if private_file(&record, code.as_bytes(), Flush::Full).is_err() {
                eprintln!("storage_backup_record_unwritten");
            }
        }
    }
    result
}

/// Removes the record of a failed upgrade backup once the database needs no backup or one
/// was made. Input: the database path. Output: none; a record that cannot be removed is
/// logged.
pub(crate) fn forget_backup_failure(path: &Path) {
    let Some(root) = path.parent() else {
        return;
    };
    match std::fs::remove_file(root.join(BACKUP_FAILURE_FILE)) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => eprintln!("storage_backup_record_kept"),
    }
}

/// Reads why the last upgrade backup of the engine at `root` failed, without an engine and
/// without following a link: only a private regular file of this user holding one known
/// code counts. Input: the engine root. Output: `storage_backup_unsafe` or
/// `storage_backup_failed`, or none.
pub fn backup_failure(root: &Path) -> Option<&'static str> {
    use std::io::Read;
    #[cfg(unix)]
    let file = {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(root.join(BACKUP_FAILURE_FILE))
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return None;
        }
        file
    };
    #[cfg(windows)]
    let file = private::read_only(&root.join(BACKUP_FAILURE_FILE)).ok()?;
    let mut text = String::new();
    file.take(64).read_to_string(&mut text).ok()?;
    [BACKUP_UNSAFE, BACKUP_FAILED]
        .into_iter()
        .find(|code| text == *code)
}

/// Writes the database and settings copies into a new backup folder: engine.db is created
/// private and empty, then filled by SQLite's backup, and each text is written behind a
/// barrier. Inputs: the connection, the folder and the settings texts. Output: Ok once every
/// copy is written; a text name that is not one plain file name fails the assert.
pub(crate) fn fill(db: &Connection, target: &Path, texts: Texts<'_>) -> Result<(), StorageError> {
    let file = target.join(DATABASE);
    drop(created(&file)?);
    db.backup(rusqlite::MAIN_DB, &file, None)
        .map_err(backup_failed)?;
    write_texts(target, texts)
}

/// Writes each settings text beside the copy of engine.db, behind a barrier. Inputs: the
/// backup folder and the texts. Output: Ok once written; a text name that is not one plain
/// file name fails the assert.
pub(crate) fn write_texts(target: &Path, texts: Texts<'_>) -> Result<(), StorageError> {
    for (name, text) in texts()? {
        assert!(
            plain_name(name),
            "a settings text is one plain file name beside engine.db and checked"
        );
        private_file(&target.join(name), text.as_bytes(), Flush::Barrier)?;
    }
    Ok(())
}

/// Whether a settings text name is one plain file name that no backup file already takes.
/// Input: the name. Output: the answer; `engine.db`, `checked`, `.`, `..` and any name with
/// a separator fail.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | ".." | DATABASE | CHECKED)
        && !name.contains(['/', '\\'])
}

/// How far a flush carries the writes before it: `Full` makes them durable on the medium,
/// `Barrier` orders them before every later write without waiting for the medium. Only
/// macOS has a barrier; elsewhere a barrier is a full flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flush {
    Barrier,
    Full,
}

#[cfg(test)]
std::thread_local! {
    /// How many full and barrier flushes this thread made, for rows that count them.
    static FLUSHES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Flushes one open file or folder handle and counts it by kind for the rows.
/// Inputs: the handle and the kind. Output: Ok once flushed, or the sync's failure.
pub(crate) fn flush(file: &std::fs::File, kind: Flush) -> std::io::Result<()> {
    #[cfg(test)]
    FLUSHES.with(|counted| {
        let (full, barrier) = counted.get();
        counted.set(match kind {
            Flush::Full => (full + 1, barrier),
            Flush::Barrier => (full, barrier + 1),
        });
    });
    match kind {
        Flush::Full => file.sync_all(),
        #[cfg(target_os = "macos")]
        Flush::Barrier => barrier(std::os::fd::AsFd::as_fd(file)),
        #[cfg(not(target_os = "macos"))]
        Flush::Barrier => file.sync_all(),
    }
}

/// Orders every write made through `fd` before any write made after it, without waiting
/// for the medium: `fcntl(F_BARRIERFSYNC)`, the one unsafe call of this crate. Input: an
/// open file or folder. Output: Ok, or the call's failure.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn barrier(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<()> {
    let done = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&fd), libc::F_BARRIERFSYNC) };
    if done == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The full and barrier flushes this thread made since the last call, which resets them.
/// Output: (full, barrier).
#[cfg(all(test, unix))]
pub(crate) fn take_flushes() -> (usize, usize) {
    FLUSHES.with(|counted| counted.replace((0, 0)))
}

/// Creates a new empty file readable only by this user. Input: the path, which must not
/// exist. Output: the open file.
pub(crate) fn created(path: &Path) -> Result<std::fs::File, StorageError> {
    #[cfg(unix)]
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(backup_failed)?;
    #[cfg(windows)]
    let file = private::file(path, true).map_err(backup_failed)?;
    Ok(file)
}

/// Creates a new file readable only by this user and writes it to disk.
/// Inputs: the path, which must not exist, the bytes and the flush that ends the write.
/// Output: Ok once flushed.
pub(crate) fn private_file(path: &Path, bytes: &[u8], kind: Flush) -> Result<(), StorageError> {
    let mut file = created(path)?;
    file.write_all(bytes).map_err(backup_failed)?;
    flush(&file, kind).map_err(backup_failed)
}

/// Removes the older backup folders: the pinned one stays outside the count, the one just
/// made stays, and of the others the newest BACKUPS_KEPT - 1 do, or BACKUPS_KEPT when the
/// one just made is the pinned one, so it is not counted twice. Only real folders of this
/// user ([`own_folder`]) named `<from>-to-<to>-<UTC time>` count; anything else, another
/// account's folder included, is neither counted nor removed. Inputs: the backups folder,
/// the new backup and the time now. Output: Ok once the older backups are gone.
fn prune(folder: &Path, made: &Path, now: SystemTime) -> Result<(), StorageError> {
    let found = backups_in(folder)?;
    let pinned = pinned(folder, &found, now);
    let made_pinned = pinned.is_some_and(|index| folder.join(&found[index].1) == made);
    let rotating: Vec<&str> = found
        .iter()
        .enumerate()
        .filter(|(index, (_, name))| Some(*index) != pinned && folder.join(name) != made)
        .map(|(_, (_, name))| name.as_str())
        .collect();
    let kept = if made_pinned {
        BACKUPS_KEPT
    } else {
        BACKUPS_KEPT - 1
    };
    let excess = rotating.len().saturating_sub(kept);
    for name in &rotating[..excess] {
        std::fs::remove_dir_all(folder.join(name)).map_err(backup_failed)?;
    }
    assert!(rotating.len() - excess <= kept);
    Ok(())
}

/// [`prune`] for a row that ages the pinned backup by hand. Inputs and output as [`prune`].
#[cfg(feature = "test-support")]
pub fn prune_for_test(folder: &Path, made: &Path, now: SystemTime) -> Result<(), StorageError> {
    prune(folder, made, now)
}

/// The real backup folders of this user, oldest first. Input: the backups folder. Output:
/// each folder's UTC time and name; links, files, other names and folders another account
/// placed there ([`own_folder`]) are left out.
pub fn backups_in(folder: &Path) -> Result<Vec<(String, String)>, StorageError> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(backup_failed)? {
        let entry = entry.map_err(backup_failed)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(time) = backup_time(&name).map(str::to_owned) else {
            continue;
        };
        if own_folder(&folder.join(&name)) {
            found.push((time, name));
        }
    }
    found.sort();
    Ok(found)
}

/// Whether a backups entry is a real folder of this user, read without following a link, so
/// a link, a file or a folder another account placed there is never counted or removed.
/// Input: the entry's path. Output: the answer, false for anything unreadable or gone.
#[cfg(unix)]
fn own_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir() && metadata.uid() == rustix::process::getuid().as_raw()
    })
}

/// Whether a backups entry is a real folder of this user, opened as itself, never a link's
/// target: a folder that is no reparse point, with this user or, for an elevated user, the
/// administrators as owner (DESIGN-0256 C6). Input: the entry's path. Output: the answer,
/// false for anything unreadable or gone, so an unknown folder is never removed.
#[cfg(windows)]
fn own_folder(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    let Ok(folder) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    else {
        return false;
    };
    let (Ok(metadata), Ok(facts), Ok(user)) = (
        folder.metadata(),
        private::facts(&folder),
        private::user_sid(),
    ) else {
        return false;
    };
    metadata.is_dir()
        && !facts.reparse
        && crazyagent_win_sandbox::dacl::trusted_owner(&facts.owner, &user)
}

/// The backup the rotation leaves alone and a restore brings back: the newest checked one a
/// 2.5 upgrade took from a 2.4 database, while its stamp is under PINNED_DAYS old, so 2.4
/// work done between a restore and the next upgrade is never lost to an older one. A stamp
/// ahead of the clock counts as young. Inputs: the backups folder, its backups oldest first
/// and the time now. Output: that backup's index, or None.
pub fn pinned(folder: &Path, found: &[(String, String)], now: SystemTime) -> Option<usize> {
    let kept_for = Duration::from_secs(86_400) * PINNED_DAYS;
    found.iter().rposition(|(time, name)| {
        let from_v24 = backup_versions(name)
            .is_some_and(|(from, to)| from <= V24_VERSION_MAX && to > V24_VERSION_MAX);
        let young = stamp_time(time)
            .is_some_and(|taken| now.duration_since(taken).map_or(true, |age| age < kept_for));
        let checked = std::fs::symlink_metadata(folder.join(name).join(CHECKED))
            .is_ok_and(|metadata| metadata.is_file());
        from_v24 && young && checked
    })
}

/// The versions in a backup folder name `<from>-to-<to>-<time>`. Input: a folder name.
/// Output: from and to, or None for any other name.
fn backup_versions(name: &str) -> Option<(i64, i64)> {
    backup_time(name)?;
    let (from, rest) = name.split_once("-to-")?;
    let (to, _) = rest.split_once('-')?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

/// The UTC time part of a backup folder name `<from>-to-<to>-<time>`.
/// Input: a folder name. Output: the time, or None for any other name.
pub fn backup_time(name: &str) -> Option<&str> {
    let (from, rest) = name.split_once("-to-")?;
    let (to, time) = rest.split_once('-')?;
    (digits(from) && digits(to) && stamp_shaped(time)).then_some(time)
}

/// Whether text is one or more ASCII digits. Input: the text. Output: the answer.
fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether text has the shape of a backup stamp such as `20261025T160700123Z`.
/// Input: the text. Output: the answer; the date itself is not checked here.
fn stamp_shaped(time: &str) -> bool {
    time.len() == 19
        && time.is_ascii()
        && digits(&time[..8])
        && &time[8..9] == "T"
        && digits(&time[9..18])
        && &time[18..] == "Z"
}

/// Reads a backup stamp back as a time, the inverse of [`stamp`]. Input: the stamp.
/// Output: the time, or None for a stamp of another shape or a date that does not exist.
pub fn stamp_time(time: &str) -> Option<SystemTime> {
    if !stamp_shaped(time) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| time[range].parse::<u32>().ok();
    let (year, month, day) = (i64::from(field(0..4)?), field(4..6)?, field(6..8)?);
    let (hour, minute, second, milli) = (
        field(9..11)?,
        field(11..13)?,
        field(13..15)?,
        field(15..18)?,
    );
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 59 {
        return None;
    }
    let count = days(year, month, day);
    if count < 0 || civil(count) != (year, month, day) {
        return None;
    }
    let seconds =
        u64::try_from(count).ok()? * 86_400 + u64::from(hour * 3_600 + minute * 60 + second);
    Some(UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(u64::from(milli)))
}

/// The day count since 1970-01-01 of a proleptic Gregorian date, the inverse of [`civil`].
/// Inputs: year, month 1-12 and day 1-31. Output: the days, negative before 1970.
fn days(year: i64, month: u32, day: u32) -> i64 {
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * shifted + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Formats a time as a UTC stamp such as `20261025T160700123Z`, which sorts in time
/// order. Input: the time. Output: the stamp, or an error outside the years 1970-9999.
pub fn stamp(time: SystemTime) -> Result<String, StorageError> {
    let since = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| backup_failed("the clock is before 1970"))?;
    let seconds = since.as_secs();
    let days =
        i64::try_from(seconds / 86_400).map_err(|_| backup_failed("the clock is out of range"))?;
    let (year, month, day) = civil(days);
    if !(1970..=9999).contains(&year) {
        return Err(backup_failed("the clock is out of range"));
    }
    let clock = seconds % 86_400;
    let text = format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}{:03}Z",
        clock / 3_600,
        clock % 3_600 / 60,
        clock % 60,
        since.subsec_millis()
    );
    assert_eq!(text.len(), 19);
    Ok(text)
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
/// Input: the days. Output: year, month 1-12 and day 1-31.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    assert!((1..=12).contains(&month) && (1..=31).contains(&day));
    (year, month, day)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use crazyagent_win_sandbox::private;
    use std::path::Path;

    /// Runs a Windows tool and fails the test when it fails. Inputs: the program and its
    /// arguments.
    fn run(program: &str, args: &[&str]) {
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "{program} {args:?} failed: {status}");
    }

    /// Lets authenticated users change a folder and what it will hold. Input: the folder.
    fn open_to_other_users(folder: &Path) {
        run(
            "icacls",
            &[folder.to_str().unwrap(), "/grant", "*S-1-5-11:(OI)(CI)(M)"],
        );
    }

    /// W1c D5 (F36): an empty backups folder of this user that other users may change is made
    /// private again, the twin of the Unix chmod; one that already holds files (#512 r2) and
    /// a junction in its place are refused as unsafe.
    #[test]
    fn an_open_backups_folder_is_repaired_and_a_junction_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        drop(private::dir(&data).unwrap());
        let backups = data.join(super::BACKUPS);
        std::fs::create_dir(&backups).unwrap();
        open_to_other_users(&backups);
        assert!(private::dir(&backups).is_err());
        assert_eq!(super::backups_folder(&data).unwrap(), backups);
        private::check(&private::dir(&backups).unwrap()).unwrap();
        std::fs::remove_dir(&backups).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        open_to_other_users(&elsewhere);
        run(
            "cmd",
            &[
                "/C",
                "mklink",
                "/J",
                backups.to_str().unwrap(),
                elsewhere.to_str().unwrap(),
            ],
        );
        let refused = super::backups_folder(&data).unwrap_err();
        assert!(refused.0.starts_with(super::BACKUP_UNSAFE), "{}", refused.0);
        assert!(private::dir(&elsewhere).is_err());
        std::fs::remove_dir(&backups).unwrap();
        std::fs::create_dir(&backups).unwrap();
        std::fs::write(backups.join("older"), "x").unwrap();
        open_to_other_users(&backups);
        let held = super::backups_folder(&data).unwrap_err();
        assert!(held.0.starts_with(super::BACKUP_UNSAFE), "{}", held.0);
        assert!(private::dir(&backups).is_err());
        std::fs::remove_dir_all(&backups).unwrap();
    }

    /// #512 r2 (rv-sec2 F1, leader 05:13): backup-shaped folders another account owns, stamped
    /// later than any backup, are neither counted nor removed by the prune, so the backup just
    /// made and this user's older ones stay.
    #[test]
    fn folders_another_account_owns_are_neither_counted_nor_removed() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        drop(private::dir(&data).unwrap());
        let backups = super::backups_folder(&data).unwrap();
        let foreign: Vec<_> = (1..=super::BACKUPS_KEPT)
            .map(|day| backups.join(format!("0-to-1-9999010{day}T000000000Z")))
            .collect();
        for folder in &foreign {
            std::fs::create_dir(folder).unwrap();
            run(
                "icacls",
                &[folder.to_str().unwrap(), "/setowner", "*S-1-5-32-545"],
            );
            assert!(!super::own_folder(folder), "{}", folder.display());
        }
        let older: Vec<_> = (1..=2)
            .map(|day| backups.join(format!("1-to-2-2000010{day}T000000000Z")))
            .collect();
        for folder in &older {
            std::fs::create_dir(folder).unwrap();
            assert!(super::own_folder(folder), "{}", folder.display());
        }
        let database = data.join(super::DATABASE);
        let connection = rusqlite::Connection::open(&database).unwrap();
        let made = super::backup(
            &connection,
            &database,
            &|| Ok(Vec::new()),
            (1, 2),
            std::time::SystemTime::now(),
        )
        .unwrap();
        assert!(made.join(super::DATABASE).is_file(), "{}", made.display());
        for folder in foreign.iter().chain(&older) {
            assert!(folder.is_dir(), "{}", folder.display());
        }
        drop(connection);
    }

    /// An upgrade backup's engine.db copy carries the full medium label engine.db carries, so a
    /// lower integrity process of the user can neither read nor hold either.
    #[test]
    fn a_backup_copy_of_the_database_carries_the_no_read_up_label() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        drop(private::dir(&data).unwrap());
        let database = data.join(super::DATABASE);
        let connection = rusqlite::Connection::open(&database).unwrap();
        let made = super::backup(
            &connection,
            &database,
            &|| Ok(Vec::new()),
            (1, 2),
            std::time::SystemTime::now(),
        )
        .unwrap();
        let copy = private::file(&made.join(super::DATABASE), false).unwrap();
        let label = private::facts(&copy)
            .unwrap()
            .label
            .and_then(|sacl| crazyagent_win_sandbox::dacl::label(&sacl).unwrap());
        assert_eq!(label, Some((0x2000, 0x7)));
        drop((copy, connection));
    }
}
