use super::canonical;
#[cfg(unix)]
use super::{current_uid, trusted_group};
use std::path::{Path, PathBuf};

/// The most links one path's resolution may pass through before the path counts as
/// changeable, as an unreadable entry does.
#[cfg(unix)]
pub(crate) const LINK_HOPS_MAX: usize = 32;

/// The first entry on a path that someone other than this user or root can change: an
/// entry another user owns, a world-writable entry such as `/tmp`, a file or folder an
/// untrusted group can write ([`trusted_group`]), or one whose access list lets someone else
/// change it, except a sticky store such as /nix/store above an entry's own folder
/// ([`first_changeable_part`]). It reads the path as written and with its links resolved; an
/// unreadable entry or access list counts. Input: the absolute path. Output: the entry.
#[cfg(unix)]
pub fn changeable_by_others(path: &Path) -> Option<PathBuf> {
    changeable(path, false)
}

/// Like [`changeable_by_others`] for the tools the resolver runs, where a sticky folder
/// others can write, such as `/tmp`, also passes as a store above an entry's own folder, and
/// a sticky store only its group can write, such as /nix/store, also as the entry's own
/// folder ([`first_changeable_part`]), since no one can replace a part this user or root
/// owns there. Input: the absolute path. Output: the entry, or none.
#[cfg(unix)]
pub fn changeable_past_sticky(path: &Path) -> Option<PathBuf> {
    changeable(path, true)
}

/// Reads a path as written and with its links resolved and names its first changeable part
/// ([`first_changeable_part`]). Inputs: the path and whether world-writable stores pass.
/// Output: the entry, or none.
#[cfg(unix)]
fn changeable(path: &Path, open_stores: bool) -> Option<PathBuf> {
    let me = current_uid();
    let resolved = canonical(path);
    let paths = [path, resolved.as_path()];
    let mut entries: Vec<&Path> = Vec::new();
    for entry in paths.into_iter().flat_map(Path::ancestors) {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let read = read_parts(&entries);
    assert_eq!(read.len(), entries.len());
    for path in paths {
        let parts: Vec<Part> = path
            .ancestors()
            .filter_map(|entry| entries.iter().position(|seen| *seen == entry))
            .map(|index| read[index])
            .collect();
        assert_eq!(parts.len(), path.ancestors().count());
        if let Some(entry) = first_changeable_part(&parts, me, open_stores)
            .and_then(|depth| path.ancestors().nth(depth))
        {
            return Some(entry.to_path_buf());
        }
    }
    None
}

/// One part of a path as the ownership rule reads it: missing, unreadable, carrying an
/// access list that lets someone other than this user or root change it ([`acl`]), or its
/// owner, group, mode and whether it is a folder or a link.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Missing,
    Unreadable,
    Shared,
    Found {
        owner: u32,
        group: u32,
        mode: u32,
        folder: bool,
        link: bool,
    },
}

/// Reads the parts of a path without following a link, every access list in one listing
/// ([`super::acl::shared_all`]). A link's own access list is never read, as its mode is not. Input:
/// the parts. Output: the reads, in order.
#[cfg(target_os = "macos")]
fn read_parts(entries: &[&Path]) -> Vec<Part> {
    use std::os::unix::fs::MetadataExt;
    let read: Vec<std::io::Result<std::fs::Metadata>> =
        entries.iter().map(std::fs::symlink_metadata).collect();
    let listed: Vec<(&Path, &std::fs::Metadata)> = entries
        .iter()
        .zip(&read)
        .filter_map(|(entry, read)| match read {
            Ok(metadata) if !metadata.file_type().is_symlink() => Some((*entry, metadata)),
            _ => None,
        })
        .collect();
    let mut shared = super::acl::shared_all(&listed).into_iter();
    let parts: Vec<Part> = read
        .iter()
        .map(|read| match read {
            Ok(metadata) => {
                let link = metadata.file_type().is_symlink();
                match (!link).then(|| shared.next().flatten()) {
                    Some(None) => Part::Unreadable,
                    Some(Some(true)) => Part::Shared,
                    Some(Some(false)) | None => Part::Found {
                        owner: metadata.uid(),
                        group: metadata.gid(),
                        mode: metadata.mode(),
                        folder: metadata.is_dir(),
                        link,
                    },
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Part::Missing,
            Err(_) => Part::Unreadable,
        })
        .collect();
    assert!(shared.next().is_none());
    assert_eq!(parts.len(), entries.len());
    parts
}

/// Reads the parts of a path one by one ([`read_part`]). Input: the parts. Output: the
/// reads, in order.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_parts(entries: &[&Path]) -> Vec<Part> {
    entries.iter().map(|entry| read_part(entry)).collect()
}

/// Reads one part of a path without following a link. A link's own access list is never
/// read, as its mode is not. Input: the part. Output: the read.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_part(entry: &Path) -> Part {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(entry) {
        Ok(metadata) => {
            let link = metadata.file_type().is_symlink();
            #[cfg(not(target_os = "macos"))]
            let shared = (!link).then(|| super::acl::shared(entry, current_uid(), trusted_group));
            match shared {
                Some(None) => return Part::Unreadable,
                Some(Some(true)) => return Part::Shared,
                Some(Some(false)) | None => {}
            }
            Part::Found {
                owner: metadata.uid(),
                group: metadata.gid(),
                mode: metadata.mode(),
                folder: metadata.is_dir(),
                link,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Part::Missing,
        Err(_) => Part::Unreadable,
    }
}

/// The first part, the entry first, that someone other than `me` or root can change. A
/// sticky folder `me` or root owns, two or more parts above the entry with the part below
/// it present, is a store: its group's write passes, and with `open_stores` everyone's
/// does too. With `open_stores` a group store also passes as the entry's own folder when
/// the entry is present, as /nix/store holds nix-darwin's brew: its group can neither
/// unlink nor rename the entry, and any file it adds there is its member's, which this rule
/// refuses. Inputs: the parts, `me` and `open_stores`. Output: the part's index, or none.
#[cfg(unix)]
pub(crate) fn first_changeable_part(parts: &[Part], me: u32, open_stores: bool) -> Option<usize> {
    let mut below_exists = false;
    for (depth, part) in parts.iter().enumerate() {
        let changeable = match *part {
            Part::Missing => false,
            Part::Unreadable | Part::Shared => true,
            Part::Found {
                owner,
                group,
                mode,
                folder,
                link,
            } => {
                let owned = owner == me || owner == 0;
                let sticky = below_exists && owned && folder && mode & 0o1000 != 0;
                let store = sticky && (depth >= 2 || (depth == 1 && open_stores));
                let open_store = sticky && depth >= 2 && open_stores;
                let world = !link && mode & 0o002 != 0;
                let grouped = !link && mode & 0o020 != 0 && !trusted_group(group);
                !owned || (world && !open_store) || (grouped && !store)
            }
        };
        if changeable {
            return Some(depth);
        }
        below_exists = *part != Part::Missing;
    }
    None
}

/// The first entry on a path that an account other than SYSTEM, the administrators,
/// TrustedInstaller and this user can change, read as written and with its links resolved
/// ([`crazyagent_win_sandbox::changeable_part`]); an unreadable entry counts. Input: the
/// absolute path. Output: the entry, or none.
#[cfg(windows)]
pub fn changeable_by_others(path: &Path) -> Option<PathBuf> {
    crazyagent_win_sandbox::changeable_part(path)
        .or_else(|| crazyagent_win_sandbox::changeable_part(&canonical(path)))
}

/// [`changeable_by_others`] for the tools the resolver runs. Windows has no sticky folder,
/// and a folder further up already passes when others may only add to it. Input: the
/// absolute path. Output: the entry, or none.
#[cfg(windows)]
pub fn changeable_past_sticky(path: &Path) -> Option<PathBuf> {
    changeable_by_others(path)
}
