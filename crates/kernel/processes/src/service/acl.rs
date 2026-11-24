use std::path::Path;

/// The `KAUTH_VNODE_*` rights that let a principal change an entry, what a folder holds, or
/// the entry's attributes, access list or owner: write data or add a file, delete, append or
/// add a folder, delete a child, write attributes, write extended attributes, write the
/// access list, take ownership, and the generic all and write rights a list may carry.
#[cfg(any(test, target_os = "macos"))]
const CHANGE_RIGHTS: u32 = (1 << 2)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 8)
    | (1 << 10)
    | (1 << 12)
    | (1 << 13)
    | (1 << 21)
    | (1 << 23);

/// The low bits of an access-list entry's flags that name its kind.
#[cfg(any(test, target_os = "macos"))]
const KIND_MASK: u32 = 0xf;

/// The kind of an access-list entry that allows its rights.
#[cfg(any(test, target_os = "macos"))]
const KIND_ALLOW: u32 = 1;

/// The kind of an access-list entry that denies its rights.
#[cfg(any(test, target_os = "macos"))]
const KIND_DENY: u32 = 2;

/// One access-list entry: the principal's GUID, the flags and the rights.
#[cfg(any(test, target_os = "macos"))]
pub(super) type Entry = ([u8; 16], u32, u32);

/// The most entries whose access-list answer is kept for the life of the process; the store
/// starts over when it fills.
#[cfg(target_os = "macos")]
const KEPT_MAX: usize = 4096;

/// An entry as it was when its access list was read: device, inode and change time, which
/// any change to its access list, mode or owner moves; to the nanosecond on APFS, to the
/// whole second on HFS+.
#[cfg(target_os = "macos")]
type Stamp = (u64, u64, i64, i64);

/// Whether an access answer read for an entry may be kept: only when its read started in
/// a later second than the entry last changed, so any later change moves the stamp even
/// where change times keep whole seconds. Inputs: the entry's stamp and the whole second
/// the read started. Output: the answer.
#[cfg(target_os = "macos")]
pub(super) fn settled(stamp: &Stamp, listed_secs: i64) -> bool {
    stamp.2 < listed_secs
}

/// Whether an access answer read for an entry may be kept: only a [`settled`] one, whose
/// entry a second `lstat` after the read found with the same device, inode and change
/// time, so no change to its access list, mode or owner fell between the two. Inputs: the
/// stamp before the read, the one after, if read, and the whole second the read started.
/// Output: the answer.
#[cfg(target_os = "macos")]
pub(super) fn keep(before: &Stamp, after: Option<&Stamp>, listed_secs: i64) -> bool {
    settled(before, listed_secs) && after == Some(before)
}

/// How many times an entry whose stamp moved during its read is read again alone before
/// two last reads decide it.
#[cfg(target_os = "macos")]
const RELIST_TRIES: u32 = 3;

/// The access-list answers read so far, by entry, each with the stamp it was read at.
#[cfg(target_os = "macos")]
static KEPT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (Stamp, bool)>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether each entry carries an access-list entry that lets someone other than this user or
/// root change it. An answer kept for an entry whose stamp has not moved is reused, and the
/// rest are read once each ([`list`]), each entry read again with `lstat` after the reads.
/// An entry whose stamp moved meanwhile, as a folder's does when a file is made in it, is
/// read again alone ([`moved_answer`]). Only answers [`keep`] allows are kept. Input: the
/// entries, none a link, each with what `lstat` read of it first. Output: one answer per
/// entry, in order; none when it cannot be read, which counts as changeable.
#[cfg(target_os = "macos")]
pub(super) fn shared_all(entries: &[(&Path, &std::fs::Metadata)]) -> Vec<Option<bool>> {
    use std::sync::PoisonError;
    let mut answers = vec![None; entries.len()];
    let mut missed = Vec::new();
    {
        let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        for (index, (entry, metadata)) in entries.iter().enumerate() {
            match kept.get(*entry) {
                Some((read, answer)) if *read == stamp(metadata) => answers[index] = Some(*answer),
                _ => missed.push(index),
            }
        }
    }
    if missed.is_empty() {
        return answers;
    }
    let listed: Vec<&Path> = missed.iter().map(|index| entries[*index].0).collect();
    let stamps: Vec<Stamp> = missed
        .iter()
        .map(|index| stamp(entries[*index].1))
        .collect();
    let listed_secs = now_secs();
    let read = list(&listed, &stamps);
    assert_eq!(read.len(), missed.len());
    let mut keeps = Vec::new();
    for (index, answer) in missed.into_iter().zip(read) {
        let (entry, metadata) = entries[index];
        let read_at = stamp(metadata);
        let after = lstat_stamp(entry);
        if after == Some(read_at) {
            answers[index] = answer;
            keeps.push((index, read_at, after, listed_secs));
        } else {
            let (answer, settled_read) = moved_answer(entry, metadata);
            answers[index] = answer;
            if let Some((before, after, secs)) = settled_read {
                keeps.push((index, before, Some(after), secs));
            }
        }
    }
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    for (index, read_at, after, secs) in keeps {
        if let Some(answer) = answers[index]
            && keep(&read_at, after.as_ref(), secs)
        {
            if kept.len() >= KEPT_MAX {
                kept.clear();
            }
            kept.insert(entries[index].0.to_path_buf(), (read_at, answer));
        }
    }
    assert!(kept.len() <= KEPT_MAX);
    assert_eq!(answers.len(), entries.len());
    answers
}

/// Reads again one entry whose stamp moved during its read: read alone up to
/// [`RELIST_TRIES`] times, an answer counting once the `lstat` before and after one read
/// agree; past that, two last reads decide, since a busy folder's change time moves with
/// every file made in it: shared when either reads shared, not shared only when both read
/// so and hold the same access-list entries, and that answer is never kept. Every read must
/// find the same file, mode and owner the caller judged from its first `lstat`, or the
/// answer is none. Inputs: the entry and that first read. Output: the answer, none when it
/// cannot be read or the entry was replaced or re-permissioned, and the stamps and start
/// second of a read that held still, for [`keep`].
#[cfg(target_os = "macos")]
fn moved_answer(
    entry: &Path,
    first: &std::fs::Metadata,
) -> (Option<bool>, Option<(Stamp, Stamp, i64)>) {
    let judged = |now: &std::fs::Metadata| {
        use std::os::unix::fs::MetadataExt;
        (now.dev(), now.ino(), now.mode(), now.uid(), now.gid())
            == (
                first.dev(),
                first.ino(),
                first.mode(),
                first.uid(),
                first.gid(),
            )
    };
    let read_now = || {
        std::fs::symlink_metadata(entry)
            .ok()
            .filter(|now| judged(now))
    };
    for _ in 0..RELIST_TRIES {
        let Some(before) = read_now() else {
            return (None, None);
        };
        let secs = now_secs();
        let answer = list(&[entry], &[stamp(&before)])[0];
        let Some(after) = read_now() else {
            return (None, None);
        };
        if stamp(&after) == stamp(&before) {
            return (answer, Some((stamp(&before), stamp(&after), secs)));
        }
    }
    let mut last = Vec::with_capacity(2);
    for _ in 0..2 {
        let Some(before) = read_now() else {
            return (None, None);
        };
        let (answers, lists) = list_entries(&[entry], &[stamp(&before)]);
        last.push((answers[0], lists.into_iter().next().flatten()));
    }
    assert_eq!(last.len(), 2);
    if read_now().is_none() {
        return (None, None);
    }
    let answer = match (&last[0], &last[1]) {
        ((Some(true), _), _) | (_, (Some(true), _)) => Some(true),
        ((Some(false), Some(first)), (Some(false), Some(second))) if first == second => Some(false),
        _ => None,
    };
    (answer, None)
}

/// Drops every kept answer for the entries, so no answer read in a call that found a part
/// others can change is used again: that part lets them swap what the parts below it name
/// while their access lists are read. Input: the entries the call read.
#[cfg(target_os = "macos")]
pub(super) fn forget(entries: &[&Path]) {
    let mut kept = KEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for entry in entries {
        kept.remove(*entry);
    }
    assert!(entries.iter().all(|entry| !kept.contains_key(*entry)));
}

/// The stamp of what `lstat` read of an entry. Input: the read. Output: the stamp.
#[cfg(target_os = "macos")]
fn stamp(metadata: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// The stamp of an entry read now with `lstat`. Input: the entry. Output: the stamp, or
/// none when it is gone or unreadable.
#[cfg(target_os = "macos")]
fn lstat_stamp(entry: &Path) -> Option<Stamp> {
    std::fs::symlink_metadata(entry).ok().map(|now| stamp(&now))
}

/// The wall clock's whole second. Output: the second, or 0 before the epoch.
#[cfg(target_os = "macos")]
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0)
}

/// Reads each entry's access list ([`list_entries`]). Inputs: the entries and their stamps.
/// Output: one answer per entry, in order.
#[cfg(target_os = "macos")]
fn list(entries: &[&Path], stamps: &[Stamp]) -> Vec<Option<bool>> {
    list_entries(entries, stamps).0
}

/// Reads each entry's access list with one `getattrlist` that follows no link
/// ([`crazyagent_macos_fs::access_list`]), and answers it only for the object its stamp names
/// ([`read_answer`]). A path with a trailing slash is read without it, so a link there is
/// read itself and never followed. Inputs: the entries and their stamps. Output: one answer
/// per entry, in order, and the entries of each list read for that object, none otherwise.
#[cfg(target_os = "macos")]
fn list_entries(
    entries: &[&Path],
    stamps: &[Stamp],
) -> (Vec<Option<bool>>, Vec<Option<Vec<Entry>>>) {
    assert_eq!(entries.len(), stamps.len());
    #[cfg(test)]
    LISTINGS.with(|listings| {
        listings
            .borrow_mut()
            .push(entries.iter().map(|entry| entry.to_path_buf()).collect())
    });
    let trusted = trusted();
    let (answers, lists): (Vec<Option<bool>>, Vec<Option<Vec<Entry>>>) = entries
        .iter()
        .zip(stamps)
        .map(|(entry, stamp)| {
            let read = c_path(entry)
                .and_then(|path| crazyagent_macos_fs::access_list(&path))
                .ok()
                .filter(|read| (read.device, read.inode) == (stamp.0, stamp.1))
                .map(|read| {
                    read.entries
                        .iter()
                        .map(|entry| (entry.principal, entry.flags, entry.rights))
                        .collect::<Vec<Entry>>()
                });
            (read_answer(read.as_deref(), trusted), read)
        })
        .unzip();
    assert_eq!(answers.len(), entries.len());
    assert_eq!(lists.len(), entries.len());
    #[cfg(test)]
    if let Some(hook) = AFTER_LISTING.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    (answers, lists)
}

/// An entry's path as a C string, without a trailing slash, which would make the read follow
/// a link at the last part. Input: the path. Output: the string, or an error for a path with
/// a NUL byte.
#[cfg(target_os = "macos")]
fn c_path(entry: &Path) -> std::io::Result<std::ffi::CString> {
    let mut bytes = entry.as_os_str().as_encoded_bytes();
    while bytes.len() > 1 && bytes.ends_with(b"/") {
        bytes = &bytes[..bytes.len() - 1];
    }
    std::ffi::CString::new(bytes).map_err(std::io::Error::other)
}

/// The answer for an entry's access list read for the object its stamp names, judged
/// against this user's and the trusted principals' GUIDs ([`entries_shared`]). Inputs: the
/// entries read, none when the read failed or found another object, and the trusted GUIDs,
/// none when they could not be read. Output: the answer, none when either is missing.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn read_answer(read: Option<&[Entry]>, trusted: Option<&[[u8; 16]]>) -> Option<bool> {
    entries_shared(read?, trusted?)
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// The entries of each access read this test thread ran, in order.
    static LISTINGS: std::cell::RefCell<Vec<Vec<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// What runs once on this test thread between the next access read and the `lstat`
    /// after it, so a row can change an entry while its access list is being read.
    static AFTER_LISTING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs the hook once on this test thread between the next access read and the `lstat`
/// after it. Input: the hook.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn after_next_listing(hook: impl FnOnce() + 'static) {
    AFTER_LISTING.with(|after| *after.borrow_mut() = Some(Box::new(hook)));
}

/// Whether an access answer is kept for the entry. Input: the entry. Output: the answer.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn kept(entry: &Path) -> bool {
    KEPT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(entry)
}

/// The entries of the last access read this test thread ran, so a hook can tell which
/// entries were just read. Output: the entries, empty before any read.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn last_listing() -> Vec<std::path::PathBuf> {
    LISTINGS.with(|listings| listings.borrow().last().cloned().unwrap_or_default())
}

/// Takes the entries of each access read this test thread ran since the last take.
/// Output: one list of entries per read, in the order they ran.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn take_listings() -> Vec<Vec<std::path::PathBuf>> {
    LISTINGS.with(|listings| std::mem::take(&mut *listings.borrow_mut()))
}

/// The GUIDs of this user, root, and the wheel, daemon and admin groups, whose members can
/// already act as root, read once. Output: the GUIDs, or none when any lookup failed, so
/// every answer is unknown.
#[cfg(target_os = "macos")]
fn trusted() -> Option<&'static [[u8; 16]]> {
    static GUIDS: std::sync::OnceLock<Option<[[u8; 16]; 5]>> = std::sync::OnceLock::new();
    GUIDS
        .get_or_init(|| {
            Some([
                crazyagent_macos_fs::user_guid(nix::unistd::getuid().as_raw()).ok()?,
                crazyagent_macos_fs::user_guid(0).ok()?,
                crazyagent_macos_fs::group_guid(0).ok()?,
                crazyagent_macos_fs::group_guid(1).ok()?,
                crazyagent_macos_fs::group_guid(80).ok()?,
            ])
        })
        .as_ref()
        .map(<[[u8; 16]; 5]>::as_slice)
}

/// Whether an access list lets someone else change its entry: an allow entry that gives a
/// change right ([`CHANGE_RIGHTS`]) to a principal whose GUID is not one of the trusted
/// ones. Deny entries and read rights never do; an entry marked only for inheritance counts
/// all the same. Inputs: the entries and the trusted GUIDs. Output: the answer, or none for
/// an entry whose kind is neither allow nor deny.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn entries_shared(entries: &[Entry], trusted: &[[u8; 16]]) -> Option<bool> {
    let mut shared = false;
    for (principal, flags, rights) in entries {
        let kind = flags & KIND_MASK;
        if kind != KIND_ALLOW && kind != KIND_DENY {
            return None;
        }
        shared |= kind == KIND_ALLOW && rights & CHANGE_RIGHTS != 0 && !trusted.contains(principal);
    }
    Some(shared)
}

/// The largest POSIX access list read, far above the entries a folder carries.
#[cfg(all(unix, not(target_os = "macos")))]
const POSIX_ACL_BYTES_MAX: usize = 8 * 1024;

/// Whether an entry carries a POSIX access-list entry that lets someone other than this
/// user or root write it, read from `system.posix_acl_access` without following a link; a
/// file system without access lists, or an entry without one, has none. Inputs: the entry,
/// this user and the trusted-group rule. Output: the answer, or none when the list cannot be
/// read, which counts as changeable.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn shared(entry: &Path, me: u32, trusted: fn(u32) -> bool) -> Option<bool> {
    use rustix::io::Errno;
    let mut value = vec![0u8; POSIX_ACL_BYTES_MAX];
    match rustix::fs::lgetxattr(entry, "system.posix_acl_access", &mut value[..]) {
        Ok(length) => posix_shared(value.get(..length)?, me, trusted),
        Err(error) if error == Errno::NODATA || error == Errno::NOTSUP => Some(false),
        Err(_) => None,
    }
}

/// The version the kernel writes at the head of a POSIX access-list value.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const POSIX_ACL_VERSION: u32 = 2;
/// A named user entry of a POSIX access list.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const POSIX_ACL_USER: u16 = 0x02;
/// A named group entry of a POSIX access list.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const POSIX_ACL_GROUP: u16 = 0x08;
/// The write right of a POSIX access-list entry.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const POSIX_ACL_WRITE: u16 = 0x02;
/// The bytes of one POSIX access-list entry: tag, rights and id.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const POSIX_ACL_ENTRY_BYTES: usize = 8;

/// Reads one `system.posix_acl_access` value: a version, then tag, rights and id entries.
/// Inputs: the value, this user and the trusted-group rule. Output: whether a named user
/// other than this user or root, or a named group the rule does not trust, has write, or
/// none for a value that does not read that way.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(super) fn posix_shared(value: &[u8], me: u32, trusted: impl Fn(u32) -> bool) -> Option<bool> {
    let (version, entries) = value.split_first_chunk::<4>()?;
    if u32::from_le_bytes(*version) != POSIX_ACL_VERSION
        || entries.len() % POSIX_ACL_ENTRY_BYTES != 0
    {
        return None;
    }
    let mut shared = false;
    for entry in entries.as_chunks::<POSIX_ACL_ENTRY_BYTES>().0 {
        let tag = u16::from_le_bytes([entry[0], entry[1]]);
        let rights = u16::from_le_bytes([entry[2], entry[3]]);
        let id = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        let other = match tag {
            POSIX_ACL_USER => id != me && id != 0,
            POSIX_ACL_GROUP => !trusted(id),
            _ => false,
        };
        shared |= other && rights & POSIX_ACL_WRITE != 0;
    }
    Some(shared)
}
