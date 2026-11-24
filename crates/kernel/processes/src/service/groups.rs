#[cfg(all(unix, any(test, not(target_os = "macos"))))]
use std::path::Path;

/// The macOS admin group, whose members can already act as root.
#[cfg(target_os = "macos")]
const ADMIN_GROUP: u32 = 80;

/// The macOS daemon group, whose only member is root; it owns /private/var/run, where
/// nix-darwin's /run link leads.
#[cfg(target_os = "macos")]
const DAEMON_GROUP: u32 = 1;

/// Whether a group that can write a folder holds only users who may already act as root:
/// wheel, daemon and admin. Every macOS user is in staff, so a staff folder is not trusted.
/// Input: the group id. Output: the answer.
#[cfg(target_os = "macos")]
pub(super) fn trusted_group(group: u32) -> bool {
    group == 0 || group == DAEMON_GROUP || group == ADMIN_GROUP
}

/// Whether a group that can write a folder holds only root or this user: root's group, or
/// this process's own group when it is this user's private group ([`own_group_private`]);
/// a shared primary group, whose other users can write the folder, is not trusted.
/// Input: the group id. Output: the answer.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn trusted_group(group: u32) -> bool {
    group == 0 || (group == rustix::process::getegid().as_raw() && own_group_private())
}

/// The largest local account file read to find a group's primary accounts.
#[cfg(all(unix, not(target_os = "macos")))]
const ACCOUNTS_BYTES_MAX: u64 = 4 * 1024 * 1024;

/// Whether this process's own group is this user's private group ([`private_group`]), read
/// once through the system's user and group lookup, which may ask a directory server, and
/// the local account file for the accounts whose primary group it is ([`primary_users`]),
/// trusted only when no directory can add accounts it lacks ([`accounts_complete`]); a
/// failed lookup or read counts as not private, so its folders fall to the stricter rule.
/// Output: the answer.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn own_group_private() -> bool {
    static PRIVATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRIVATE.get_or_init(|| {
        let gid = nix::unistd::getegid();
        let user = nix::unistd::User::from_uid(nix::unistd::getuid())
            .ok()
            .flatten();
        let group = nix::unistd::Group::from_gid(gid).ok().flatten();
        let primary = local_accounts()
            .filter(|_| accounts_complete(Path::new("/etc")))
            .and_then(|text| primary_users(&text, gid.as_raw()));
        match (user, group, primary) {
            (Some(user), Some(group), Some(primary)) => {
                private_group(&group.name, &group.mem, &primary, &user.name)
            }
            _ => false,
        }
    })
}

/// Reads the local account file, `/etc/passwd`, at most [`ACCOUNTS_BYTES_MAX`]; a larger or
/// unreadable file gives none. Output: the text, or none.
#[cfg(all(unix, not(target_os = "macos")))]
fn local_accounts() -> Option<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open("/etc/passwd")
        .ok()?
        .take(ACCOUNTS_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= ACCOUNTS_BYTES_MAX).then_some(text)
}

/// The largest user lookup configuration read to find where accounts come from.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
const LOOKUP_CONFIG_BYTES_MAX: u64 = 64 * 1024;

/// The most entries read from SSSD's drop-in folder before it counts as configured.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
const SSSD_DROP_INS_MAX: usize = 4096;

/// Whether the local account file holds every account the system's user lookup can give a
/// primary group: every `passwd` source in `nsswitch.conf` is `files`, `compat` or
/// `systemd`, or `sss` while SSSD has no configuration ([`sssd_unconfigured`]). Action
/// items and comments are skipped; a missing or unreadable file, a larger one or no
/// `passwd` line gives no. Input: the folder holding `nsswitch.conf` and `sssd`, `/etc`
/// outside rows. Output: the answer.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
pub(super) fn accounts_complete(etc: &Path) -> bool {
    use std::io::Read as _;
    let mut text = String::new();
    let read = std::fs::File::open(etc.join("nsswitch.conf")).and_then(|file| {
        file.take(LOOKUP_CONFIG_BYTES_MAX + 1)
            .read_to_string(&mut text)
    });
    if read.is_err() || text.len() as u64 > LOOKUP_CONFIG_BYTES_MAX {
        return false;
    }
    let mut lines = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let Some(sources) = line.trim_start().strip_prefix("passwd:") else {
            continue;
        };
        lines += 1;
        let mut in_action = false;
        for word in sources.split_whitespace() {
            if in_action || word.starts_with('[') {
                in_action = !word.ends_with(']');
                continue;
            }
            let local = match word {
                "files" | "compat" | "systemd" => true,
                "sss" => sssd_unconfigured(&etc.join("sssd")),
                _ => false,
            };
            if !local {
                return false;
            }
        }
    }
    assert!(lines <= text.len());
    lines > 0
}

/// Whether SSSD has no configuration, so it serves no directory: `sssd.conf` is missing and
/// the `conf.d` drop-in folder is missing or holds no `.conf` entry. Any other answer, such
/// as a folder this user cannot look into, counts as configured. Input: the SSSD folder.
/// Output: the answer.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn sssd_unconfigured(sssd: &Path) -> bool {
    let missing = |error: &std::io::Error| error.kind() == std::io::ErrorKind::NotFound;
    match std::fs::metadata(sssd.join("sssd.conf")) {
        Err(error) if missing(&error) => {}
        _ => return false,
    }
    let entries = match std::fs::read_dir(sssd.join("conf.d")) {
        Ok(entries) => entries,
        Err(error) => return missing(&error),
    };
    for (seen, entry) in entries.enumerate() {
        if seen == SSSD_DROP_INS_MAX {
            return false;
        }
        let Ok(entry) = entry else {
            return false;
        };
        if entry.file_name().as_encoded_bytes().ends_with(b".conf") {
            return false;
        }
    }
    true
}

/// Whether a group named `name`, listing `members`, is the user private group of the user
/// named `user`: it bears the user's name, lists no other member and is the primary group of
/// this user's account alone, as Debian, Ubuntu and Fedora make one per user; a shared
/// primary group such as openSUSE's `users` is not, and neither is one whose user the local
/// account file does not list. Inputs: the group's name, its listed members, the accounts
/// whose primary group it is and the user's name. Output: the answer.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(super) fn private_group(
    name: &str,
    members: &[String],
    primary: &[String],
    user: &str,
) -> bool {
    !user.is_empty()
        && name == user
        && members.iter().all(|member| member == user)
        && primary.iter().any(|account| account == user)
        && primary.iter().all(|account| account == user)
}

/// The accounts in a local account file (`/etc/passwd` lines) whose primary group is `gid`,
/// which the group's member list leaves out. Inputs: the file's text and the group id.
/// Output: the account names, or none when a line cannot be read or names accounts kept
/// elsewhere.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(super) fn primary_users(passwd: &str, gid: u32) -> Option<Vec<String>> {
    let mut accounts = Vec::new();
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('+') || line.starts_with('-') {
            return None;
        }
        let mut fields = line.split(':');
        let name = fields.next()?;
        let primary: u32 = fields.nth(2)?.parse().ok()?;
        if primary == gid {
            accounts.push(name.to_owned());
        }
    }
    assert!(accounts.len() <= passwd.len());
    Some(accounts)
}
