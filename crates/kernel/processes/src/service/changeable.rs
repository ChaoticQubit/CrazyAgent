use super::canonical;
#[cfg(unix)]
use super::{current_uid, trusted_group};
use std::path::{Path, PathBuf};

/// The most links one path's resolution may pass through before the path counts as
/// changeable, as an unreadable entry does.
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

/// Reads a path as written, with its links resolved, and every link, `..` folder and first
/// missing part its resolution passes through ([`link_chain`]), and names the first
/// changeable part of any of them ([`first_changeable_part`]). A link that cannot be read,
/// or a chain past [`LINK_HOPS_MAX`], counts as changeable. A call that finds a changeable
/// part keeps no access answer it read. Inputs: the path and whether world-writable stores
/// pass. Output: the entry, or none.
#[cfg(unix)]
fn changeable(path: &Path, open_stores: bool) -> Option<PathBuf> {
    let me = current_uid();
    let resolved = canonical(path);
    let hops = match link_chain(path) {
        Ok(hops) => hops,
        Err(entry) => return Some(entry),
    };
    let mut paths: Vec<&Path> = vec![path, resolved.as_path()];
    paths.extend(hops.iter().map(PathBuf::as_path));
    let mut entries: Vec<&Path> = Vec::new();
    for entry in paths.iter().flat_map(|path| path.ancestors()) {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let read = read_parts(&entries);
    assert_eq!(read.len(), entries.len());
    for path in &paths {
        let parts: Vec<Part> = path
            .ancestors()
            .filter_map(|entry| entries.iter().position(|seen| *seen == entry))
            .map(|index| read[index])
            .collect();
        assert_eq!(parts.len(), path.ancestors().count());
        if let Some(entry) = first_changeable_part(&parts, me, open_stores)
            .and_then(|depth| path.ancestors().nth(depth))
        {
            #[cfg(target_os = "macos")]
            super::acl::forget(&entries);
            return Some(entry.to_path_buf());
        }
    }
    None
}

/// Every link a path's resolution passes through, named where it sits, every folder a `..`
/// in it leaves, and the first missing part where it would sit, since whoever can write its
/// folder can make it, in the order the resolution meets them. The missing part ends the
/// walk, since nothing past it exists to follow. Input: an absolute path. Output: those places,
/// or the entry that makes the path changeable: one that cannot be read, or the link past
/// [`LINK_HOPS_MAX`], or one whose target names no start ([`link_start`]). A relative path,
/// or one on no drive root, is changeable as a whole, since what it names depends on the
/// working folder or on a share.
fn link_chain(path: &Path) -> Result<Vec<PathBuf>, PathBuf> {
    use std::path::Component;
    let Some(mut at) = link_start(path, path).filter(|_| path.is_absolute()) else {
        return Err(path.to_path_buf());
    };
    let queue = |pending: &mut Vec<PathBuf>, from: &Path| {
        for part in from.components().rev() {
            match part {
                Component::Normal(name) => pending.push(PathBuf::from(name)),
                Component::ParentDir => pending.push(PathBuf::from("..")),
                Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            }
        }
    };
    let mut pending = Vec::new();
    queue(&mut pending, path);
    let mut hops = Vec::new();
    let mut links = 0;
    while let Some(part) = pending.pop() {
        if part.as_os_str() == ".." {
            if at.parent().is_some() {
                hops.push(at.clone());
            }
            at.pop();
            continue;
        }
        let next = at.join(&part);
        let kind = match std::fs::symlink_metadata(&next) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hops.push(next);
                break;
            }
            Err(_) => return Err(next),
        };
        if !kind.is_symlink() {
            at = next;
            continue;
        }
        if links == LINK_HOPS_MAX {
            return Err(next);
        }
        links += 1;
        let Ok(target) = std::fs::read_link(&next) else {
            return Err(next);
        };
        let Some(start) = link_start(&at, &target) else {
            return Err(next);
        };
        hops.push(next);
        at = start;
        queue(&mut pending, &target);
    }
    assert!(links <= LINK_HOPS_MAX);
    assert!(hops.len() >= links);
    Ok(hops)
}

/// Where the walk of a link's target starts: the root of the drive a target with a drive
/// and root names, the root of the drive the walk is on for a target with a root and no
/// drive, and the link's own folder for a relative target. Any other target (a share, a
/// volume or device name, or a drive with no root) names none, so the link counts as
/// changeable. Inputs: the link's folder and the target. Output: the folder, or none.
pub(super) fn link_start(at: &Path, target: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let mut parts = target.components();
    match parts.next() {
        Some(Component::Prefix(prefix)) => match (prefix.kind(), parts.next()) {
            (Prefix::Disk(drive) | Prefix::VerbatimDisk(drive), Some(Component::RootDir)) => {
                Some(PathBuf::from(format!("{}:\\", char::from(drive))))
            }
            _ => None,
        },
        Some(Component::RootDir) => at.ancestors().last().map(Path::to_path_buf),
        _ => Some(at.to_path_buf()),
    }
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
/// TrustedInstaller and this user can change, read as written, with its links resolved, and
/// every link, junction, `..` folder and first missing part its resolution passes through
/// ([`link_chain`], [`crazyagent_win_sandbox::changeable_part`]); an unreadable entry, a link
/// that cannot be read or followed from a drive root, or a chain past [`LINK_HOPS_MAX`]
/// counts. Input: the absolute path. Output: the entry, or none.
#[cfg(windows)]
pub fn changeable_by_others(path: &Path) -> Option<PathBuf> {
    let hops = match link_chain(path) {
        Ok(hops) => hops,
        Err(entry) => return Some(entry),
    };
    [path.to_path_buf(), canonical(path)]
        .iter()
        .chain(&hops)
        .find_map(|part| crazyagent_win_sandbox::changeable_part(part))
}

/// [`changeable_by_others`] for the tools the resolver runs. Windows has no sticky folder,
/// and a folder further up already passes when others may only add to it. Input: the
/// absolute path. Output: the entry, or none.
#[cfg(windows)]
pub fn changeable_past_sticky(path: &Path) -> Option<PathBuf> {
    changeable_by_others(path)
}

/// The first part of a socket's path, the socket first, that someone other than this user
/// or root can change, read as [`changeable_past_sticky`] reads a path but with the group and
/// world write bits of the socket and its folder set aside: a socket's write bit only lets a
/// caller connect, and the caller judges the folder's mode itself. Owners, sticky bits,
/// access lists and every folder above still count. Input: the canonical socket path.
#[cfg(unix)]
pub fn changeable_above_socket(socket: &Path) -> Option<PathBuf> {
    assert!(socket.is_absolute());
    let entries: Vec<&Path> = socket.ancestors().collect();
    let mut parts = read_parts(&entries);
    assert_eq!(parts.len(), entries.len());
    for part in parts.iter_mut().take(2) {
        if let Part::Found { mode, .. } = part {
            *mode &= !0o022;
        }
    }
    first_changeable_part(&parts, current_uid(), true)
        .and_then(|depth| socket.ancestors().nth(depth))
        .map(Path::to_path_buf)
}
