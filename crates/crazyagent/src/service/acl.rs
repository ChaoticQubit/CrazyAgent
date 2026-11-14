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
const TRUSTED_PRINCIPALS: [&str; 3] = ["user:root", "group:wheel", "group:admin"];

/// The longest a macOS access listing may take.
#[cfg(target_os = "macos")]
const LIST_TIME: std::time::Duration = std::time::Duration::from_secs(5);

/// The most entries whose access-list answer is kept for the life of the process; the store
/// starts over when it fills.
#[cfg(target_os = "macos")]
const KEPT_MAX: usize = 4096;

/// An entry as it was when its access list was read: device, inode and change time to the
/// nanosecond, which any change to its access list, mode or owner moves.
#[cfg(target_os = "macos")]
type Stamp = (u64, u64, i64, i64);

/// The access-list answers read so far, by entry, each with the stamp it was read at.
#[cfg(target_os = "macos")]
static KEPT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (Stamp, bool)>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether each entry carries an access-list entry that lets someone other than this user or
/// root change it. An answer kept for an entry whose stamp has not moved is reused, and the
/// rest are read in one `/bin/ls -lde` ([`list`]); only answers read are kept. Input: the
/// entries, none a link, each with what `lstat` read of it first. Output: one answer per
/// entry, in order; none when it cannot be read, which counts as changeable.
#[cfg(target_os = "macos")]
pub(super) fn shared_all(entries: &[(&Path, &std::fs::Metadata)]) -> Vec<Option<bool>> {
    use std::sync::PoisonError;
    /// The stamp of what `lstat` read of an entry. Input: the read. Output: the stamp.
    fn stamp(metadata: &std::fs::Metadata) -> Stamp {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }
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
    let read = list(&listed);
    assert_eq!(read.len(), missed.len());
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    for (index, answer) in missed.into_iter().zip(read) {
        answers[index] = answer;
        if let Some(answer) = answer {
            if kept.len() >= KEPT_MAX {
                kept.clear();
            }
            let (entry, metadata) = entries[index];
            kept.insert(entry.to_path_buf(), (stamp(metadata), answer));
        }
    }
    assert!(kept.len() <= KEPT_MAX);
    assert_eq!(answers.len(), entries.len());
    answers
}

/// Lists entries in one `/bin/ls -lde` that follows no link and reads each one's block
/// ([`listed_shared_all`]). A name that is not text or holds a line break gets none, and so
/// does every entry when the listing fails, runs out of time or fills the output bound, as a
/// cut listing could drop access-list lines. Input: the entries. Output: one answer per
/// entry, in order.
#[cfg(target_os = "macos")]
fn list(entries: &[&Path]) -> Vec<Option<bool>> {
    let names: Vec<Option<&str>> = entries
        .iter()
        .map(|entry| entry.to_str().filter(|name| !name.contains('\n')))
        .collect();
    let operands: Vec<&str> = names.iter().flatten().copied().collect();
    if operands.is_empty() {
        return vec![None; entries.len()];
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
        .arg("-lde")
        .arg("--")
        .args(&operands);
    let text = match crate::setup::process::run_command(command, LIST_TIME) {
        Ok((true, output)) if output.len() < crate::setup::process::OUTPUT_BYTES_MAX => {
            String::from_utf8_lossy(&output).into_owned()
        }
        _ => return vec![None; entries.len()],
    };
    let mut read = listed_shared_all(&text, &operands, own_name()).into_iter();
    let answers: Vec<Option<bool>> = names
        .iter()
        .map(|name| name.and_then(|_| read.next().flatten()))
        .collect();
    assert!(read.next().is_none());
    assert_eq!(answers.len(), entries.len());
    answers
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// The entries of each access listing this test thread ran, in order.
    static LISTINGS: std::cell::RefCell<Vec<Vec<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(Vec::new()) };
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

/// Reads one `ls -lde` listing of several entries block by block: an entry's line, then its
/// access-list lines, each indented ([`listed_shared`]). A block goes to the one entry its
/// first line ends with, after a space, the longest when one name ends another. Inputs: the
/// listing, the entries as listed and this user's name, if known. Output: one answer per
/// entry, in order; none for an entry with no block or two, or whose block does not read,
/// and none for every entry when a block names no entry or an access-list line comes first.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn listed_shared_all(
    text: &str,
    entries: &[&str],
    me: Option<&str>,
) -> Vec<Option<bool>> {
    let none = vec![None; entries.len()];
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') {
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
        answers[index] = listed_shared(&block.join("\n"), me);
    }
    for (answer, count) in answers.iter_mut().zip(&blocks_of) {
        if *count != 1 {
            *answer = None;
        }
    }
    assert_eq!(answers.len(), entries.len());
    answers
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
    for entry in entries.chunks_exact(POSIX_ACL_ENTRY_BYTES) {
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
