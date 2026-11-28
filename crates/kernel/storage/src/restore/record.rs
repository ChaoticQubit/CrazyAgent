use super::{RESTORE_RECORD_FILE, restore_failed};
use crate::StorageError;
use crate::migrations::{Flush, created, flush};
use rustix::fs::{FlockOperation, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The bytes of one slot: one 4 KiB block, so a torn block write breaks at most one slot.
pub(super) const SLOT_BYTES: usize = 4096;
/// The bytes of the whole record file: slot A at 0, slot B at 4096.
pub(super) const RECORD_BYTES: usize = 2 * SLOT_BYTES;
/// Where each slot starts in the file.
const SLOT_OFFSETS: [u64; 2] = [0, 4096];
/// The bytes a slot starts with, the last one the format.
const MAGIC: &[u8; 5] = b"CARR\x01";
/// The magic, the u64 seq and the u16 line length that come before the line.
const HEADER_BYTES: usize = 15;
/// The sha256 that follows the line.
const DIGEST_BYTES: usize = 32;
/// The most a line holds, from the record's own limit.
const LINE_BYTES_MAX: usize = super::RESTORE_RECORD_BYTES_MAX;
/// The most an old one-file record holds.
const OLD_BYTES_MAX: u64 = 512;
/// The temp file the first write of a root fills before it renames it over the record.
pub(super) const RECORD_TEMP: &str = "restore-record.new";
/// The private file whose lock every record write holds.
pub(super) const RECORD_LOCK: &str = "restore-record.lock";
/// How long a write waits for the record lock before it is refused as busy.
pub(super) const LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long a write sleeps between two tries of the lock.
const LOCK_POLL: Duration = Duration::from_millis(10);
/// The cause a write gives when another write held the record lock for all of its wait.
pub(super) const LOCK_HELD: &str = "the restore record lock is held by another write";
/// The cause a read or write of a record that cannot be read gives.
pub(super) const UNREADABLE: &str = "the restore record cannot be read";

/// The most record files this process remembers as having their folder entry on disk: an
/// engine has one root, and a test process makes about a hundred. Past it the set starts over,
/// which costs each root one more folder flush, never a lost one.
pub(super) const ROOTS_MAX: usize = 256;

/// The record files, by device and inode, whose folder entry this process fully flushed
/// after the rename that named them, so a later write knows its name is on disk.
static FOLDER_FLUSHED: Mutex<BTreeSet<(u64, u64)>> = Mutex::new(BTreeSet::new());

/// The step that writes one slot's bytes at its offset, so a row can tear it.
pub(super) type Place = fn(&File, u64, &[u8]) -> std::io::Result<()>;

#[cfg(test)]
std::thread_local! {
    /// Whether each record write on this thread ended on its own full flush, in order.
    static WRITES: std::cell::RefCell<Vec<bool>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
std::thread_local! {
    /// Whether the next first write of a root on this thread fails its folder flush after
    /// the rename; the write that reads it sets it back to false.
    static FAIL_FOLDER_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the next first write of a root on this thread fail its folder flush after the
/// rename, so a row can meet a renamed record whose folder entry is not on disk. Input:
/// none. Output: none.
#[cfg(test)]
pub(super) fn fail_next_folder_flush() {
    FAIL_FOLDER_FLUSH.with(|fail| fail.set(true));
}

/// Takes the list of this thread's record writes, each true when it ended on its own full
/// flush, and starts a new one. Output: the list.
#[cfg(test)]
pub(super) fn take_writes() -> Vec<bool> {
    WRITES.with(|writes| writes.take())
}

/// The record file as found: missing, in the old one-file format, or as its two slots.
enum Found {
    Missing,
    Old(String),
    Slots(File, [Option<(u64, String)>; 2]),
}

/// Writes one slot's bytes at its offset in one pwrite; a short write is an error and is
/// not tried again. Inputs: the file, the offset and the bytes. Output: Ok once written.
pub(super) fn place(file: &File, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
    assert_eq!(bytes.len(), SLOT_BYTES);
    let written = file.write_at(bytes, offset)?;
    if written != bytes.len() {
        return Err(std::io::Error::other("short restore record write"));
    }
    Ok(())
}

/// One slot's bytes: the magic, the seq, the line's length, the line, its sha256 and zeros
/// to the end of the block. Inputs: the seq and the line. Output: the slot.
pub(super) fn encoded(seq: u64, line: &str) -> Vec<u8> {
    assert!(line.len() <= LINE_BYTES_MAX);
    let length = u16::try_from(line.len()).unwrap_or(u16::MAX);
    assert_eq!(usize::from(length), line.len());
    let mut slot = Vec::with_capacity(SLOT_BYTES);
    slot.extend_from_slice(MAGIC);
    slot.extend_from_slice(&seq.to_le_bytes());
    slot.extend_from_slice(&length.to_le_bytes());
    slot.extend_from_slice(line.as_bytes());
    let digest = Sha256::digest(&slot);
    slot.extend_from_slice(&digest);
    slot.resize(SLOT_BYTES, 0);
    assert_eq!(slot.len(), SLOT_BYTES);
    slot
}

/// A slot's seq and line, or none for a slot that is torn, partial or not a slot: a wrong
/// magic, a line over the limit, a sha256 that does not match, bytes after it that are not
/// zero, or a line that is not UTF-8. Input: the slot's bytes. Output: the seq and line.
fn decoded(slot: &[u8]) -> Option<(u64, String)> {
    assert_eq!(slot.len(), SLOT_BYTES);
    if slot[..MAGIC.len()] != MAGIC[..] {
        return None;
    }
    let seq = u64::from_le_bytes(slot[5..13].try_into().ok()?);
    let length = usize::from(u16::from_le_bytes(slot[13..15].try_into().ok()?));
    if length > LINE_BYTES_MAX {
        return None;
    }
    let end = HEADER_BYTES + length;
    let digest = Sha256::digest(&slot[..end]);
    if slot[end..end + DIGEST_BYTES] != digest[..] {
        return None;
    }
    if slot[end + DIGEST_BYTES..].iter().any(|byte| *byte != 0) {
        return None;
    }
    let line = String::from_utf8(slot[HEADER_BYTES..end].to_vec()).ok()?;
    Some((seq, line))
}

/// The newer of the two slots: the one valid slot, or of two the one ahead by serial-number
/// order, so the seq wraps at u64::MAX. Input: the decoded slots. Output: its index, seq and
/// line, or Err when neither is valid or both hold one seq.
fn newest(slots: &[Option<(u64, String)>; 2]) -> Result<(usize, u64, &str), ()> {
    match (&slots[0], &slots[1]) {
        (Some(a), Some(b)) => {
            if a.0 == b.0 {
                return Err(());
            }
            if b.0.wrapping_sub(a.0) < 1 << 63 {
                Ok((1, b.0, &b.1))
            } else {
                Ok((0, a.0, &a.1))
            }
        }
        (Some(a), None) => Ok((0, a.0, &a.1)),
        (None, Some(b)) => Ok((1, b.0, &b.1)),
        (None, None) => Err(()),
    }
}

/// Opens the record of the engine at `root` without following a link and reads it: only a
/// private regular file of this user of exactly two slots, or of 1 to 512 bytes without the
/// slot magic in the old format, counts. Inputs: the root and whether to open it for writing.
/// Output: what was found, or Err for any other file or failure.
fn found(root: &Path, write: bool) -> Result<Found, ()> {
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
        .open(root.join(RESTORE_RECORD_FILE));
    let file = match opened {
        Ok(file) => file,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(Found::Missing),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(());
    }
    let size = metadata.len();
    if size == RECORD_BYTES as u64 {
        let mut bytes = vec![0; RECORD_BYTES];
        file.read_exact_at(&mut bytes, 0).map_err(|_| ())?;
        let slots = [decoded(&bytes[..SLOT_BYTES]), decoded(&bytes[SLOT_BYTES..])];
        return Ok(Found::Slots(file, slots));
    }
    if !(1..=OLD_BYTES_MAX).contains(&size) {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(OLD_BYTES_MAX)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    assert!(bytes.len() as u64 <= OLD_BYTES_MAX);
    if bytes.starts_with(&MAGIC[..4]) {
        return Err(());
    }
    String::from_utf8(bytes).map(Found::Old).map_err(|_| ())
}

/// Reads the record of the engine at `root`: the newer valid slot's line, none for an empty
/// slot or a missing file, or an old one-file record's text. Input: the root. Output: the
/// line, or Err when the file cannot be read or neither slot is valid.
pub(super) fn read(root: &Path) -> Result<Option<String>, ()> {
    match found(root, false)? {
        Found::Missing => Ok(None),
        Found::Old(text) => Ok(Some(text)),
        Found::Slots(_, slots) => {
            let (_, _, line) = newest(&slots)?;
            Ok((!line.is_empty()).then(|| line.to_owned()))
        }
    }
}

/// The record lock of one write: an exclusive flock on a fresh open of the lock file, given
/// back by an explicit unlock, so a copy of the descriptor that a spawned process still
/// holds never keeps the next write out.
pub(super) struct RecordLock(pub(super) File);

impl Drop for RecordLock {
    fn drop(&mut self) {
        while rustix::fs::flock(&self.0, FlockOperation::Unlock) == Err(Errno::INTR) {}
    }
}

/// Takes the record lock of the engine at `root`: opens the lock file close-on-exec without
/// following a link, refuses one that is not a private regular file of this user, then waits
/// for the lock. Input: the root. Output: the lock, or `storage_restore_failed` naming the
/// held lock or the failure.
pub(super) fn locked(root: &Path) -> Result<RecordLock, StorageError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
        .open(root.join(RECORD_LOCK))
        .map_err(restore_failed)?;
    let metadata = file.metadata().map_err(restore_failed)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(restore_failed(
            "the restore record lock is not a private file of this user",
        ));
    }
    waited(&file)?;
    Ok(RecordLock(file))
}

/// Tries the exclusive lock until it is taken or [`LOCK_WAIT`] has passed, so a process that
/// holds it for good refuses the write instead of hanging the start. Input: the lock file.
/// Output: Ok once locked, or `storage_restore_failed` naming the held lock or the failure.
fn waited(file: &File) -> Result<(), StorageError> {
    let started = Instant::now();
    loop {
        match rustix::fs::flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(Errno::WOULDBLOCK) => {
                if started.elapsed() >= LOCK_WAIT {
                    return Err(restore_failed(LOCK_HELD));
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(cause) => return Err(restore_failed(cause)),
        }
    }
}

/// Replaces the record of the engine at `root` with one line, empty for no record, under the
/// record lock: the older slot is written in place and, when asked, fully flushed. A missing
/// or old one-file record is made whole once by [`first`]. A record that cannot be read, or
/// whose slots are both torn, is refused. Inputs: the root, the line, whether to flush and
/// the slot writer. Output: Ok once written.
pub(super) fn write(
    root: &Path,
    line: &str,
    flushed: bool,
    put: Place,
) -> Result<(), StorageError> {
    assert!(line.len() <= LINE_BYTES_MAX);
    let _lock = locked(root)?;
    let written = match found(root, true).map_err(|()| restore_failed(UNREADABLE))? {
        Found::Slots(file, slots) => {
            let (index, seq, _) = newest(&slots).map_err(|()| restore_failed(UNREADABLE))?;
            let older = 1 - index;
            put(
                &file,
                SLOT_OFFSETS[older],
                &encoded(seq.wrapping_add(1), line),
            )
            .map_err(restore_failed)?;
            if flushed {
                flush(&file, Flush::Full).map_err(restore_failed)?;
                named_on_disk(root, &file)?;
            }
            flushed
        }
        Found::Missing | Found::Old(_) => {
            first(root, line)?;
            true
        }
    };
    assert!(written || !flushed);
    #[cfg(test)]
    WRITES.with(|writes| writes.borrow_mut().push(written));
    Ok(())
}

/// The cause of a failed step as it stands, for the one restore failure that wraps it.
/// Input: the cause. Output: it as a storage error.
fn failed(cause: std::io::Error) -> StorageError {
    StorageError(cause.to_string())
}

/// Makes the two-slot record of the engine at `root` whole: slot A holds the line at seq 1
/// and slot B is empty, in a private temp file that is fully flushed, renamed over the record,
/// and its folder fully flushed; the temp file is removed after any failure. Inputs: the root
/// and the line. Output: Ok once on disk.
fn first(root: &Path, line: &str) -> Result<(), StorageError> {
    use std::io::Write;
    let record = root.join(RESTORE_RECORD_FILE);
    let temp = root.join(RECORD_TEMP);
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(cause) => return Err(restore_failed(cause)),
    }
    let mut bytes = encoded(1, line);
    bytes.resize(RECORD_BYTES, 0);
    let made = created(&temp)
        .and_then(|mut file| {
            file.write_all(&bytes).map_err(failed)?;
            flush(&file, Flush::Full).map_err(failed)?;
            key(&file).map_err(failed)
        })
        .and_then(|key| {
            std::fs::rename(&temp, &record).map_err(failed)?;
            Ok(key)
        });
    let key = match made {
        Ok(key) => key,
        Err(cause) => {
            let _ = std::fs::remove_file(&temp);
            return Err(restore_failed(cause.0));
        }
    };
    forget_flushed(key);
    #[cfg(test)]
    if FAIL_FOLDER_FLUSH.with(|fail| fail.replace(false)) {
        return Err(restore_failed("the record folder flush failed"));
    }
    File::open(root)
        .and_then(|folder| flush(&folder, Flush::Full))
        .map_err(restore_failed)?;
    remember_flushed(key);
    Ok(())
}

/// The device and inode of a record file, the key of [`FOLDER_FLUSHED`]. Input: the open
/// file. Output: the key, or the failure to read its metadata.
fn key(file: &File) -> std::io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Makes sure the folder entry naming the record file is on disk before a flushed write
/// returns: unless this process already fully flushed it, the root folder is fully flushed
/// now, with the set's lock dropped across the flush. Inputs: the root and the open record
/// file. Output: Ok once the name is on disk, or `storage_restore_failed`.
fn named_on_disk(root: &Path, file: &File) -> Result<(), StorageError> {
    let key = key(file).map_err(restore_failed)?;
    if flushed_set().contains(&key) {
        return Ok(());
    }
    File::open(root)
        .and_then(|folder| flush(&folder, Flush::Full))
        .map_err(restore_failed)?;
    remember_flushed(key);
    Ok(())
}

/// The set of record files whose folder entry is on disk, locked; a poisoned lock is taken
/// as it is, since the set only ever saves a flush. Output: the guard.
fn flushed_set() -> std::sync::MutexGuard<'static, BTreeSet<(u64, u64)>> {
    FOLDER_FLUSHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Records that a record file's folder entry is on disk. Input: the file's key. Output: none.
fn remember_flushed(key: (u64, u64)) {
    remembered(&mut flushed_set(), key);
}

/// Adds a key to a set of flushed record files, starting the set over when it already holds
/// [`ROOTS_MAX`] other keys, so it never grows past the cap. Inputs: the set and the key.
/// Output: none.
pub(super) fn remembered(set: &mut BTreeSet<(u64, u64)>, key: (u64, u64)) {
    if set.len() >= ROOTS_MAX && !set.contains(&key) {
        set.clear();
    }
    set.insert(key);
    assert!(set.len() <= ROOTS_MAX);
}

/// Drops a record file from the set before its folder flush, so an inode reused from a file
/// gone earlier never counts as flushed. Input: the file's key. Output: none.
fn forget_flushed(key: (u64, u64)) {
    flushed_set().remove(&key);
}
