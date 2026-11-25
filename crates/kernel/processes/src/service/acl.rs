use std::path::Path;

/// The access-list rights that let a principal change an entry, what a folder holds, or the
/// entry's attributes, access list or owner, as macOS `ls -e` names them.
#[cfg(any(test, target_os = "macos"))]
const CHANGE_RIGHTS: [&str; 10] = [
    "write",
    "append",
    "add_file",
    "add_subdirectory",
    "delete",
    "delete_child",
    "writeattr",
    "writeextattr",
    "writesecurity",
    "chown",
];

/// The macOS principals that may change an entry without making it changeable by others:
/// root and the groups whose members can already act as root, as `trusted_group` reads them.
#[cfg(any(test, target_os = "macos"))]
const TRUSTED_PRINCIPALS: [&str; 4] = ["user:root", "group:wheel", "group:daemon", "group:admin"];

/// The longest a macOS access listing may take.
#[cfg(target_os = "macos")]
const LIST_TIME: std::time::Duration = std::time::Duration::from_secs(5);

/// The most entries whose access-list answer is kept for the life of the process; the store
/// starts over when it fills.
#[cfg(target_os = "macos")]
const KEPT_MAX: usize = 4096;

/// An entry as it was when its access list was read: device, inode and change time, which
/// any change to its access list, mode or owner moves; to the nanosecond on APFS, to the
/// whole second on HFS+.
#[cfg(target_os = "macos")]
type Stamp = (u64, u64, i64, i64);

/// Whether an access answer read for an entry may be kept: only when its listing started in
/// a later second than the entry last changed, so any later change moves the stamp even
/// where change times keep whole seconds (HFS-CTIME). Inputs: the entry's stamp and the
/// whole second the listing started. Output: the answer.
#[cfg(target_os = "macos")]
pub(super) fn settled(stamp: &Stamp, listed_secs: i64) -> bool {
    stamp.2 < listed_secs
}

/// Whether an access answer read for an entry may be kept: only a [`settled`] one, whose
/// entry a second `lstat` after the listing found with the same device, inode and change
/// time, so no change to its access list, mode or owner fell between the two. Inputs: the
/// stamp before the listing, the one after, if read, and the whole second the listing
/// started. Output: the answer.
#[cfg(target_os = "macos")]
pub(super) fn keep(before: &Stamp, after: Option<&Stamp>, listed_secs: i64) -> bool {
    settled(before, listed_secs) && after == Some(before)
}

/// How many times an entry whose stamp moved during its listing is listed again alone
/// before one last direct read decides it.
#[cfg(target_os = "macos")]
const RELIST_TRIES: u32 = 3;

/// The access-list answers read so far, by entry, each with the stamp it was read at.
#[cfg(target_os = "macos")]
static KEPT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (Stamp, bool)>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether each entry carries an access-list entry that lets someone other than this user or
/// root change it. An answer kept for an entry whose stamp has not moved is reused, and the
/// rest are read in one `/bin/ls -lide` ([`list`]), each entry read again with `lstat` after
/// the listing. An entry whose stamp moved meanwhile, as a folder's does when a file is made
/// in it, is read again alone ([`moved_answer`]). Only answers [`keep`] allows are kept.
/// Input: the entries, none a link, each with what `lstat` read of it first. Output: one
/// answer per entry, in order; none when it cannot be read, which counts as changeable.
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
    let inodes: Vec<u64> = missed
        .iter()
        .map(|index| std::os::unix::fs::MetadataExt::ino(entries[*index].1))
        .collect();
    let listed_secs = now_secs();
    let read = list(&listed, &inodes);
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

/// Reads again one entry whose stamp moved during its listing: listed alone up to
/// [`RELIST_TRIES`] times, an answer counting once the `lstat` before and after one listing
/// agree; past that, two last listings decide, since a busy folder's change time moves with
/// every file made in it: shared when either reads shared, not shared only when both read
/// so and print the same access-list lines, and that answer is never kept. Every read must
/// find the same file, mode and owner the caller judged from its first `lstat`, or the
/// answer is none. Inputs: the entry and that first read. Output: the answer, none when it
/// cannot be read or the entry was replaced or re-permissioned, and the stamps and start
/// second of a listing that held still, for [`keep`].
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
        let answer = list(&[entry], &[std::os::unix::fs::MetadataExt::ino(&before)])[0];
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
        let (answers, text) = list_text(&[entry], &[std::os::unix::fs::MetadataExt::ino(&before)]);
        last.push((answers[0], text.map(|text| access_lines(&text))));
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

/// The access-list lines of an `ls -lide` listing, each ` <n>: `, in order; the first line
/// of each block is left out, since its size and dates move with every file made in a
/// folder. Input: the listing. Output: the lines.
#[cfg(target_os = "macos")]
fn access_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| access_line(line))
        .map(str::to_owned)
        .collect()
}

/// Says whether a listing line is an access-list line, ` <n>: ` with digits only before the
/// colon, rather than an entry's first line. Input: the line. Output: true for an
/// access-list line.
#[cfg(any(test, target_os = "macos"))]
fn access_line(line: &str) -> bool {
    line.starts_with(' ')
        && line.trim_start().split_once(':').is_some_and(|(index, _)| {
            !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Whether an entry still sits at its path as the same inode, read after a direct read that
/// found no access list, since that read also finds none for a path that is gone or whose
/// folder moved away. Inputs: the path and the inode it was listed as. Output: true when it
/// is there as that inode.
#[cfg(target_os = "macos")]
fn still_there(entry: &Path, inode: u64) -> bool {
    std::fs::symlink_metadata(entry)
        .is_ok_and(|now| std::os::unix::fs::MetadataExt::ino(&now) == inode)
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

/// Lists entries in one `/bin/ls -lide` that follows no link and reads each one's block
/// against the inode the entry was stamped with ([`listed_shared_all`]). A name that is not
/// text or holds a line break gets none, and so does every entry when the listing fails,
/// runs out of time or fills the output bound, as a cut listing could drop access-list
/// lines. Inputs: the entries and their inodes. Output: one answer per entry, in order.
#[cfg(target_os = "macos")]
fn list(entries: &[&Path], inodes: &[u64]) -> Vec<Option<bool>> {
    list_text(entries, inodes).0
}

/// [`list`], with the listing's text as `ls` printed it. Inputs: the entries and their
/// inodes. Output: one answer per entry, in order, and the text, none when the listing did
/// not run, failed, ran out of time or filled the output bound.
#[cfg(target_os = "macos")]
fn list_text(entries: &[&Path], inodes: &[u64]) -> (Vec<Option<bool>>, Option<String>) {
    assert_eq!(entries.len(), inodes.len());
    let names: Vec<Option<&str>> = entries
        .iter()
        .map(|entry| entry.to_str().filter(|name| !name.contains('\n')))
        .collect();
    let operands: Vec<&str> = names.iter().flatten().copied().collect();
    let listed_inodes: Vec<u64> = names
        .iter()
        .zip(inodes)
        .filter(|(name, _)| name.is_some())
        .map(|(_, inode)| *inode)
        .collect();
    if operands.is_empty() {
        return (vec![None; entries.len()], None);
    }
    #[cfg(test)]
    LISTINGS.with(|listings| {
        listings
            .borrow_mut()
            .push(operands.iter().map(std::path::PathBuf::from).collect())
    });
    let mut command = std::process::Command::new("/bin/ls");
    command
        .env_clear()
        .env("LC_ALL", "C")
        .arg("-lide")
        .arg("--")
        .args(&operands);
    let text = match crate::run::run_command(command, LIST_TIME) {
        Ok((true, output)) if output.len() < crate::run::OUTPUT_BYTES_MAX => {
            String::from_utf8_lossy(&output).into_owned()
        }
        _ => return (vec![None; entries.len()], None),
    };
    let read = listed_shared_all(&text, &operands, &listed_inodes, own_name(), &|index| {
        confirmed(
            std::ffi::CString::new(operands[index])
                .map_err(std::io::Error::other)
                .and_then(|path| crazyagent_macos_fs::acl_listed(&path)),
        )
        .filter(|_| still_there(Path::new(operands[index]), listed_inodes[index]))
    });
    let mut read = read.into_iter();
    let answers: Vec<Option<bool>> = names
        .iter()
        .map(|name| name.and_then(|_| read.next().flatten()))
        .collect();
    assert!(read.next().is_none());
    assert_eq!(answers.len(), entries.len());
    #[cfg(test)]
    if let Some(hook) = AFTER_LISTING.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    (answers, Some(text))
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// The entries of each access listing this test thread ran, in order.
    static LISTINGS: std::cell::RefCell<Vec<Vec<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// What runs once on this test thread between the next access listing and the `lstat`
    /// after it, so a row can change an entry while its access list is being read.
    static AFTER_LISTING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs the hook once on this test thread between the next access listing and the `lstat`
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

/// The entries of the last access listing this test thread ran, so a hook can tell which
/// entries were just listed. Output: the entries, empty before any listing.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn last_listing() -> Vec<std::path::PathBuf> {
    LISTINGS.with(|listings| listings.borrow().last().cloned().unwrap_or_default())
}

/// Takes the entries of each access listing this test thread ran since the last take.
/// Output: one list of entries per listing, in the order they ran.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn take_listings() -> Vec<Vec<std::path::PathBuf>> {
    LISTINGS.with(|listings| std::mem::take(&mut *listings.borrow_mut()))
}

/// This user's short name as access listings print it, read once. Output: the name, or none
/// when the lookup fails, so every user entry counts as someone else's.
#[cfg(target_os = "macos")]
fn own_name() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        nix::unistd::User::from_uid(nix::unistd::getuid())
            .ok()
            .flatten()
            .map(|user| user.name)
    })
    .as_deref()
}

/// Reads one `ls -lde` listing: the entry's line, then one ` <n>: <principal> [inherited]
/// allow|deny <rights>` line per access-list entry. Inputs: the listing and this user's
/// name, if known. Output: whether an allow entry gives a change right ([`CHANGE_RIGHTS`])
/// to a principal other than this user or [`TRUSTED_PRINCIPALS`], or none for a listing
/// that does not read that way.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn listed_shared(text: &str, me: Option<&str>) -> Option<bool> {
    let mut lines = text.lines();
    lines.next()?;
    let own = me.map(|me| format!("user:{me}"));
    let mut shared = false;
    for line in lines {
        let (index, entry) = line.trim_start().split_once(':')?;
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let words: Vec<&str> = entry.split_whitespace().collect();
        let principal = words.first()?;
        let kind = words
            .iter()
            .position(|word| *word == "allow" || *word == "deny")?;
        let rights = words.get(kind + 1)?;
        let trusted = TRUSTED_PRINCIPALS.contains(principal) || own.as_deref() == Some(*principal);
        let changes = rights
            .split(',')
            .any(|right| CHANGE_RIGHTS.contains(&right));
        shared |= words[kind] == "allow" && changes && !trusted;
    }
    Some(shared)
}

/// Reads one `ls -lide` listing of several entries block by block: an entry's line, its
/// inode first, padded to one width, then its access-list lines, each ` <n>: `
/// ([`listed_shared`]). A block goes to the one entry its first line ends with, after a
/// space, the longest when one name ends another; a block whose inode is not the entry's,
/// or does not parse, is about another file and reads none, never asked again. A block of
/// one line that reads as not shared takes the answer of `unlisted` for its entry instead,
/// since `ls` prints a list it could not read exactly as no list. Inputs: the listing, the
/// entries as listed, their inodes, this user's name, if known, and the reader asked again
/// by an entry's index. Output: one answer per entry, in order; none for an entry with no block or two, or
/// whose block does not read, and none for every entry when a block names no entry or an
/// access-list line comes first.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn listed_shared_all(
    text: &str,
    entries: &[&str],
    inodes: &[u64],
    me: Option<&str>,
    unlisted: &dyn Fn(usize) -> Option<bool>,
) -> Vec<Option<bool>> {
    assert_eq!(entries.len(), inodes.len());
    let none = vec![None; entries.len()];
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if !access_line(line) {
            blocks.push(vec![line]);
            continue;
        }
        match blocks.last_mut() {
            Some(block) => block.push(line),
            None => return none,
        }
    }
    let mut answers = none.clone();
    let mut blocks_of = vec![0usize; entries.len()];
    for block in &blocks {
        let named = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                block[0]
                    .strip_suffix(**entry)
                    .is_some_and(|rest| rest.ends_with(' '))
            })
            .max_by_key(|(_, entry)| entry.len());
        let Some((index, _)) = named else {
            return none;
        };
        blocks_of[index] += 1;
        let inode = block[0]
            .split_whitespace()
            .next()
            .and_then(|field| field.parse::<u64>().ok());
        if inode != Some(inodes[index]) {
            answers[index] = None;
            continue;
        }
        answers[index] = listed_shared(&block.join("\n"), me);
        if block.len() == 1 && answers[index] == Some(false) {
            answers[index] = unlisted(index);
        }
    }
    for (answer, count) in answers.iter_mut().zip(&blocks_of) {
        if *count != 1 {
            *answer = None;
        }
    }
    assert_eq!(answers.len(), entries.len());
    answers
}

/// What an entry that `ls -lde` printed no access-list lines for has, as read again
/// directly: none confirmed stays none, while a list that is there or cannot be read is not
/// known. Input: the direct read. Output: the answer.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn confirmed(read: std::io::Result<bool>) -> Option<bool> {
    matches!(read, Ok(false)).then_some(false)
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
