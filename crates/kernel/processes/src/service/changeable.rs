use super::canonical;
#[cfg(unix)]
use super::{current_uid, trusted_group};
use std::path::{Path, PathBuf};

/// What one read of a place found without following a link ([`read_place`]): on macOS one
/// `getattrlist` reply carrying the access list too, elsewhere what `lstat` found.
#[cfg(target_os = "macos")]
type Read = crazyagent_macos_fs::HeldEntry;

/// What one read of a place found without following a link ([`read_place`]): on macOS one
/// `getattrlist` reply carrying the access list too, elsewhere what `lstat` found.
#[cfg(not(target_os = "macos"))]
type Read = std::fs::Metadata;

/// What [`read_place`] read of each place a link walk found, by the place's path as the walk
/// named it.
type Known = Vec<(PathBuf, Read)>;

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
/// missing part its resolution passes through ([`link_chain_read`]), and names the first
/// changeable part of any of them ([`first_changeable_part`]). A link that cannot be read,
/// or a chain past [`LINK_HOPS_MAX`], counts as changeable. Inputs: the path and whether
/// world-writable stores pass. Output: the entry, or none.
#[cfg(unix)]
fn changeable(path: &Path, open_stores: bool) -> Option<PathBuf> {
    let me = current_uid();
    let resolved = canonical(path);
    let (hops, known) = match link_chain_read(path) {
        Ok(chain) => chain,
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
    let read = read_parts_known(&entries, &known);
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
            return Some(entry.to_path_buf());
        }
    }
    None
}

/// The places of [`link_chain_read`] without its reads. Input: an absolute path. Output:
/// the places, or the entry that makes the path changeable.
#[cfg(windows)]
fn link_chain(path: &Path) -> Result<Vec<PathBuf>, PathBuf> {
    link_chain_read(path).map(|(hops, _)| hops)
}

/// Every link a path's resolution passes through, named where it sits, every folder a `..`
/// in it leaves, and the first missing part where it would sit, since whoever can write its
/// folder can make it, in the order the resolution meets them. The missing part ends the
/// walk, since nothing past it exists to follow. Input: an absolute path. Output: those places,
/// or the entry that makes the path changeable: one that cannot be read, or the link past
/// [`LINK_HOPS_MAX`], or one whose target names no start ([`link_start`]). A relative path,
/// or one on no drive root, is changeable as a whole, since what it names depends on the
/// working folder or on a share.
/// Each place the walk found comes with what [`read_place`] read of it, named as the walk
/// named it, so a caller that reads those places again in the same call can take these reads
/// instead ([`read_parts_known`]).
fn link_chain_read(path: &Path) -> Result<(Vec<PathBuf>, Known), PathBuf> {
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
    let mut known: Known = Vec::new();
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
        let link = match read_place(&next) {
            Ok(read) => {
                let link = is_link(&read);
                known.push((next.clone(), read));
                link
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hops.push(next);
                break;
            }
            Err(_) => return Err(next),
        };
        if !link {
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
    assert!(known.len() >= links);
    Ok((hops, known))
}

/// Reads one place without following a link, with one `getattrlistat` by its absolute name
/// that brings owner, mode, kind and access list in one reply. A name that is not absolute or
/// holds a NUL cannot be read. Input: the place. Output: the read, or the error.
#[cfg(target_os = "macos")]
fn read_place(place: &Path) -> std::io::Result<Read> {
    let name = place_name(place)?;
    crazyagent_macos_fs::held_entry_at(None, &name)
}

/// A place's absolute name as [`crazyagent_macos_fs::held_entry_at`] takes it with no folder
/// ([`super::acl::c_path`]). Input: the place. Output: the name, or an error for a place that
/// is not absolute or holds a NUL.
#[cfg(target_os = "macos")]
fn place_name(place: &Path) -> std::io::Result<std::ffi::CString> {
    if !place.is_absolute() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let name = super::acl::c_path(place)?;
    assert_eq!(name.as_bytes().first(), Some(&b'/'));
    Ok(name)
}

/// Reads one place without following a link (`lstat`). Input: the place. Output: the read,
/// or the error.
#[cfg(not(target_os = "macos"))]
fn read_place(place: &Path) -> std::io::Result<Read> {
    std::fs::symlink_metadata(place)
}

/// Whether a read found a link. Input: the read. Output: the answer.
#[cfg(target_os = "macos")]
fn is_link(read: &Read) -> bool {
    read.link()
}

/// Whether a read found a link. Input: the read. Output: the answer.
#[cfg(not(target_os = "macos"))]
fn is_link(read: &Read) -> bool {
    read.file_type().is_symlink()
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

/// Reads the parts of a path without following a link, each part's access list read for the
/// object `lstat` found ([`super::acl::shared_all`]). A link's own access list is never read,
/// as its mode is not. Input: the parts. Output: the reads, in order.
#[cfg(target_os = "macos")]
pub(super) fn read_parts(entries: &[&Path]) -> Vec<Part> {
    use std::os::unix::fs::MetadataExt;
    let read: Vec<std::io::Result<std::fs::Metadata>> = entries
        .iter()
        .map(|entry| std::fs::symlink_metadata(entry))
        .collect();
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

/// Reads the parts of a path for one check, each with one read by its absolute name that
/// follows no link ([`read_place`]), or the read `known` holds for that same path from earlier
/// in the call ([`link_chain_read`]); owner, mode, kind and access list come from that one
/// reply ([`held_part`]), each volume asked once per device. Inputs: the parts and the reads
/// already made. Output: the reads, in order.
#[cfg(target_os = "macos")]
fn read_parts_known(entries: &[&Path], known: &[(PathBuf, Read)]) -> Vec<Part> {
    let mut volumes: Volumes = Vec::new();
    let parts: Vec<Part> = entries
        .iter()
        .map(|entry| {
            held_part(read_known(entry, known), &mut volumes, |device| {
                place_name(entry)
                    .and_then(|name| {
                        crazyagent_macos_fs::held_volume_keeps_lists_at(None, &name, device)
                    })
                    .unwrap_or(false)
            })
        })
        .collect();
    assert!(volumes.len() <= parts.len());
    assert_eq!(parts.len(), entries.len());
    parts
}

/// Reads the parts of a path one by one ([`read_part`]). Input: the parts. Output: the
/// reads, in order.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_parts(entries: &[&Path]) -> Vec<Part> {
    read_parts_known(entries, &[])
}

/// [`read_parts`], taking for an entry the read `known` already holds for that same path,
/// made by `lstat` earlier in the same call ([`link_chain_read`]). Inputs: the parts and the
/// reads already made. Output: the reads, in order.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_parts_known(entries: &[&Path], known: &[(PathBuf, Read)]) -> Vec<Part> {
    entries
        .iter()
        .map(|entry| read_part(entry, read_known(entry, known)))
        .collect()
}

/// What [`read_place`] reads of an entry: the read `known` holds for that exact path, else a
/// new read. Inputs: the entry and the reads already made. Output: the read.
#[cfg(unix)]
fn read_known(entry: &Path, known: &[(PathBuf, Read)]) -> std::io::Result<Read> {
    match known.iter().find(|(place, _)| place.as_path() == entry) {
        Some((place, read)) => {
            assert_eq!(place.as_path(), entry);
            Ok(read.clone())
        }
        None => read_place(entry),
    }
}

/// Reads one part of a path without following a link. A link's own access list is never
/// read, as its mode is not. Inputs: the part and what `lstat` read of it. Output: the read.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_part(entry: &Path, read: std::io::Result<std::fs::Metadata>) -> Part {
    use std::os::unix::fs::MetadataExt;
    match read {
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

/// The parts of a socket's path, the socket first, read in one walk down from `/` through
/// held folder handles ([`held_parts`]), or with `lstat` per part ([`read_parts`]) where the
/// path is not a plain chain of names or `/` cannot be opened. Inputs: the socket and its
/// ancestors, the socket first. Output: one part per ancestor, in that order.
#[cfg(target_os = "macos")]
fn socket_parts(socket: &Path, entries: &[&Path]) -> Vec<Part> {
    match held_parts(socket) {
        Some(mut parts) => {
            parts.reverse();
            parts
        }
        None => read_parts(entries),
    }
}

/// The parts of a socket's path, read with `lstat` per part ([`read_parts`]). Inputs: the
/// socket and its ancestors, the socket first. Output: one part per ancestor, in that order.
#[cfg(all(unix, not(target_os = "macos")))]
fn socket_parts(_socket: &Path, entries: &[&Path]) -> Vec<Part> {
    read_parts(entries)
}

/// Reads every part of an absolute path from `/` down in one walk: each folder is opened for
/// search with no link followed and read through that handle with one `getattrlist`, so the
/// object judged is the object walked into and no name is looked up twice. A link mid-path
/// is read itself and then followed, as `lstat` follows it for the parts below; the last part
/// is read through its folder without following a link. A part that cannot be opened is read
/// itself, and every part below it reads missing when the open found nothing there and
/// unreadable otherwise, as `lstat` reads them. Input: the path. Output: one part per
/// ancestor, `/` first, or none for a path with a part other than a name, or when `/` cannot
/// be opened.
#[cfg(target_os = "macos")]
pub(super) fn held_parts(path: &Path) -> Option<Vec<Part>> {
    use std::os::fd::AsFd;
    use std::path::Component;
    let mut names = Vec::new();
    for (index, part) in path.components().enumerate() {
        match part {
            Component::RootDir if index == 0 => {}
            Component::Normal(name) => {
                names.push(std::ffi::CString::new(name.as_encoded_bytes()).ok()?);
            }
            _ => return None,
        }
    }
    let mut places: Vec<&Path> = path.ancestors().collect();
    places.reverse();
    if places.len() != names.len() + 1 {
        return None;
    }
    let mut held = crazyagent_macos_fs::open_search(None, c"/", false).ok()?;
    let mut parts = Vec::with_capacity(places.len());
    let mut volumes: Volumes = Vec::new();
    let read = crazyagent_macos_fs::held_entry(held.as_fd());
    parts.push(held_part(read, &mut volumes, |device| {
        crazyagent_macos_fs::held_volume_keeps_lists(held.as_fd(), device).unwrap_or(false)
    }));
    for (index, name) in names.iter().enumerate() {
        let at = |held: &std::os::fd::OwnedFd, volumes: &mut Volumes| {
            let read = crazyagent_macos_fs::held_entry_at(Some(held.as_fd()), name);
            held_part(read, volumes, |device| {
                crazyagent_macos_fs::held_volume_keeps_lists_at(Some(held.as_fd()), name, device)
                    .unwrap_or(false)
            })
        };
        if index + 1 == names.len() {
            parts.push(at(&held, &mut volumes));
            break;
        }
        let failed = match crazyagent_macos_fs::open_search(Some(held.as_fd()), name, false) {
            Ok(child) => {
                let read = crazyagent_macos_fs::held_entry(child.as_fd());
                parts.push(held_part(read, &mut volumes, |device| {
                    crazyagent_macos_fs::held_volume_keeps_lists(child.as_fd(), device)
                        .unwrap_or(false)
                }));
                held = child;
                continue;
            }
            Err(error) => error,
        };
        let part = at(&held, &mut volumes);
        parts.push(part);
        let below = if matches!(part, Part::Found { link: true, .. }) {
            match crazyagent_macos_fs::open_search(Some(held.as_fd()), name, true) {
                Ok(child) => {
                    held = child;
                    continue;
                }
                Err(error) => error,
            }
        } else {
            failed
        };
        let rest = if below.kind() == std::io::ErrorKind::NotFound {
            Part::Missing
        } else {
            Part::Unreadable
        };
        parts.resize(places.len(), rest);
        break;
    }
    assert!(volumes.len() <= parts.len());
    assert_eq!(parts.len(), places.len());
    Some(parts)
}

/// The volumes one held walk has asked about, by device, each with whether it keeps access
/// lists; it lives only for that walk.
#[cfg(target_os = "macos")]
type Volumes = Vec<(u64, bool)>;

/// One part as a held read gives it: missing, unreadable, carrying an access list others can
/// change it through ([`super::acl::held_shared`], never read for a link, as [`read_parts`]
/// reads none), or its owner, group, permission bits and kind. The volume is asked once per
/// device within the walk. Inputs: the read, the walk's volume answers and the volume
/// question. Output: the part.
#[cfg(target_os = "macos")]
fn held_part(
    read: std::io::Result<crazyagent_macos_fs::HeldEntry>,
    volumes: &mut Volumes,
    volume: impl FnOnce(u64) -> bool,
) -> Part {
    let read = match read {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Part::Missing,
        Err(_) => return Part::Unreadable,
    };
    let link = read.link();
    if !link {
        let keeps = match volumes.iter().find(|(device, _)| *device == read.device) {
            Some((_, keeps)) => *keeps,
            None => {
                let keeps = volume(read.device);
                volumes.push((read.device, keeps));
                keeps
            }
        };
        match super::acl::held_shared(&read, keeps) {
            None => return Part::Unreadable,
            Some(true) => return Part::Shared,
            Some(false) => {}
        }
    }
    Part::Found {
        owner: read.owner,
        group: read.group,
        mode: read.mode & 0o7777,
        folder: read.folder(),
        link,
    }
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
    let mut parts = socket_parts(socket, &entries);
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
