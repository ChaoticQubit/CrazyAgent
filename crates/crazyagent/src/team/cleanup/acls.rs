use super::*;

/// Clears the extended ACL of each entry without following a link in its last part, through
/// `/bin/chmod -f -h -N` on its path, as [`release_acls`] does. A deny entry an agent set
/// binds the owner too, so it would keep an entry from going or being written (RV-DS241-137
/// sec r1 F2). Only a folder or an entry with one link is cleared, so
/// a file with a second link, which may be the owner's file an agent linked in, never
/// changes (V231-ACL, sec r2 F1). Input: the entries. An entry that is gone or skipped, a
/// pinned one or a spawn that fails leaves things as they were, and the step that needs the
/// entry then reports it.
#[cfg(target_os = "macos")]
pub(crate) fn clear_acls<'a>(entries: impl IntoIterator<Item = &'a Path>) {
    use std::os::unix::fs::MetadataExt;
    let cleared = |entry: &&Path| {
        std::fs::symlink_metadata(entry).is_ok_and(|found| found.is_dir() || found.nlink() == 1)
    };
    let cleared: Vec<&Path> = entries.into_iter().filter(cleared).collect();
    chmod_acls(&["-f", "-h", "-N", "--"], &cleared);
}

/// Leaves ACLs alone where no deny entry an agent may set keeps an entry from going. Input:
/// unused.
#[cfg(not(target_os = "macos"))]
pub(crate) fn clear_acls<'a>(_entries: impl IntoIterator<Item = &'a Path>) {}

/// Clears the extended ACL of each entry itself for a step that stopped on it, as a launch
/// context's release clears what a tree holds: a folder and a one-link file by their paths,
/// so a folder whose ACL denies listing or searching and a file whose ACL denies reading
/// lose it too, where [`clear_acls`] opens them and fails, and a one-link link on the link
/// itself. A file with a second link, which may be the owner's file an agent linked in,
/// never changes (V231-ACL, RV-DS241-137 code r2 F3). A name a process swaps for a link
/// between the check and the clear is followed, as in that release. Input: the entries.
/// What stays is reported by the step that needs the entry.
#[cfg(target_os = "macos")]
pub(crate) fn release_acls<'a>(entries: impl IntoIterator<Item = &'a Path>) {
    use std::os::unix::fs::MetadataExt;
    let mut by_path = Vec::new();
    let mut links = Vec::new();
    for entry in entries {
        let Ok(found) = std::fs::symlink_metadata(entry) else {
            continue;
        };
        if found.is_symlink() {
            if found.nlink() == 1 {
                links.push(entry);
            }
        } else if found.is_dir() || found.nlink() == 1 {
            by_path.push(entry);
        }
    }
    chmod_acls(&["-f", "-N", "--"], &by_path);
    chmod_acls(&["-f", "-h", "-N", "--"], &links);
}

/// Leaves ACLs alone where no deny entry an agent may set keeps an entry from going. Input:
/// unused.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn release_acls<'a>(_entries: impl IntoIterator<Item = &'a Path>) {}

/// Runs `/bin/chmod` with `flags`, which end in `--`, on the entries, its output dropped,
/// and nothing when there are none. Inputs: the flags and the entries. A spawn that fails
/// changes nothing.
#[cfg(target_os = "macos")]
fn chmod_acls(flags: &[&str], entries: &[&Path]) {
    use std::process::Stdio;
    assert_eq!(flags.last(), Some(&"--"));
    if entries.is_empty() {
        return;
    }
    let _ = kernel_processes::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .args(flags)
            .args(entries)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}
