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

/// Whether an entry carries an access-list entry that lets someone other than this user or
/// root change it, read from `/bin/ls -lde` without following a link. Input: the entry.
/// Output: the answer, or none when the listing cannot be read, which counts as changeable.
#[cfg(target_os = "macos")]
pub(super) fn shared(entry: &Path) -> Option<bool> {
    let mut command = std::process::Command::new("/bin/ls");
    command
        .env_clear()
        .env("LC_ALL", "C")
        .arg("-lde")
        .arg(entry);
    #[cfg(test)]
    LISTINGS.with(|listings| listings.borrow_mut().push(vec![entry.to_path_buf()]));
    let (ok, output) = crate::setup::process::run_command(command, LIST_TIME).ok()?;
    if !ok || output.len() >= crate::setup::process::OUTPUT_BYTES_MAX {
        return None;
    }
    listed_shared(&String::from_utf8_lossy(&output), own_name())
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

/// Reads one `ls -lde` listing of several entries block by block. Inputs: the listing, the
/// entries as listed and this user's name, if known. Output: one answer per entry, in order.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn listed_shared_all(
    text: &str,
    entries: &[&str],
    me: Option<&str>,
) -> Vec<Option<bool>> {
    let _ = (text, me);
    vec![None; entries.len()]
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
