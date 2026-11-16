use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The environment variable naming the signed-in user's home folder.
#[cfg(unix)]
pub(super) const OWNER_HOME_VARIABLE: &str = "HOME";

/// The environment variable naming the signed-in user's home folder.
/// Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) const OWNER_HOME_VARIABLE: &str = "USERPROFILE";

/// The disk a folder is on, to tell a quick rename from a copy across disks.
/// Unix: the device number of what the path names. Output: None when unreadable.
#[cfg(unix)]
pub(super) fn volume(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.dev())
}

/// The disk a folder is on. Windows: not read, so a Move there is never called
/// a quick rename up front. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn volume(_path: &Path) -> Option<u64> {
    None
}

/// Permission bits compared between a source and its copy.
/// Unix: the mode's permission, setuid, setgid and sticky bits.
#[cfg(unix)]
pub(super) fn permission_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

/// Permission bits compared between a source and its copy.
/// Windows: only the read-only flag exists. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn permission_bits(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

/// What tells an entry left as it was from one changed or replaced between a move's check
/// of its copy and the delete of its source: a write gives it a new size or modify time, and
/// a rename over gives the path a new inode. The change time is left out, since deleting one
/// hard link of a file changes it for every other link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
}

impl Stamp {
    /// Tells whether the entry was last modified less than `window` before `now`, or after
    /// it. Such a stamp proves nothing: a file system that stamps from a coarse clock, or in
    /// whole seconds, gives a later same-size write the same modify time. Inputs: the time
    /// of the check and the window. Output: the answer.
    pub(super) fn modified_since(&self, now: std::time::SystemTime, window: Duration) -> bool {
        const SECOND_NS: i128 = 1_000_000_000;
        let Ok(now) = now.duration_since(std::time::UNIX_EPOCH) else {
            return true;
        };
        let modified = i128::from(self.modified.0) * SECOND_NS + i128::from(self.modified.1);
        let (now, window) = (now.as_nanos() as i128, window.as_nanos() as i128);
        assert!(now >= 0 && window >= 0);
        modified + window > now
    }
}

/// The stamp of an entry. Unix: its device, inode, size and modify time to the nanosecond.
/// Input: the entry's own metadata, links not followed. Output: the stamp.
#[cfg(unix)]
pub(super) fn stamp(metadata: &fs::Metadata) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    Some(Stamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.size(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
    })
}

/// The stamp of an entry. Windows: its size and modify time; none when the time is
/// unreadable, so that entry is never deleted. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn stamp(metadata: &fs::Metadata) -> Option<Stamp> {
    let since = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(Stamp {
        device: 0,
        inode: 0,
        bytes: metadata.len(),
        modified: (
            i64::try_from(since.as_secs()).ok()?,
            i64::from(since.subsec_nanos()),
        ),
    })
}

/// Makes a folder being filled by a copy private to the owner and writable.
/// The folder's real permissions are applied after its contents are copied.
#[cfg(unix)]
pub(super) fn private_folder(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Makes a folder being filled by a copy writable.
/// Windows: folders inherit their parent's access list. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn private_folder(path: &Path) -> std::io::Result<()> {
    let mut permissions = fs::symlink_metadata(path)?.permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "On Windows this clears only FILE_ATTRIBUTE_READONLY; access stays with the inherited ACL"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

/// Lets the owner delete inside a folder: adds owner read, write and search, never
/// through a link put at its path ([`set_mode`]).
#[cfg(unix)]
pub(super) fn writable_folder(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)?.permissions().mode();
    set_mode(path, fs::Permissions::from_mode(mode | 0o700))
}

/// Lets the owner delete inside a folder by clearing its read-only flag.
/// Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn writable_folder(path: &Path) -> std::io::Result<()> {
    private_folder(path)
}

/// Recreates the symlink at `original` as the same link at `copy`, never
/// following it. Input: the original link, its target text and the new path.
#[cfg(unix)]
pub(super) fn copy_link(_original: &Path, target: PathBuf, copy: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, copy)
}

/// Recreates the symlink at `original` as the same link at `copy`, never
/// following it; Windows keeps file and folder links apart. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn copy_link(original: &Path, target: PathBuf, copy: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if fs::symlink_metadata(original)?.file_type().is_symlink_dir() {
        std::os::windows::fs::symlink_dir(target, copy)
    } else {
        std::os::windows::fs::symlink_file(target, copy)
    }
}

/// A source folder a copy reads from, opened from the copy's root one plain name at a time
/// with no link followed in any part and checked to be the folder its listing showed, so a
/// folder swapped for a link is refused, never read through (RV-DS241-41). Unix: the open
/// folder, each entry read relative to it.
#[cfg(unix)]
pub(super) struct SourceFolder(fs::File);

/// A source folder a copy reads from. Windows: its path, checked to be a real folder when
/// opened, each entry read by path as before. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) struct SourceFolder(PathBuf);

#[cfg(unix)]
impl SourceFolder {
    /// Opens the folder `below` a copy's root and checks it is the folder `seen` describes.
    /// Inputs: the root, the plain names below it (empty for the root itself) and the
    /// folder's metadata from its listing. Output: the folder, or the I/O error, also for a
    /// folder replaced since it was listed.
    pub(super) fn open(root: &Path, below: &Path, seen: &fs::Metadata) -> std::io::Result<Self> {
        use rustix::fs::OFlags;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let folder = fs::File::from(crate::platform::open_beneath(root, below, flags)?);
        same_entry(&folder, seen)?;
        Ok(Self(folder))
    }

    /// The names in the folder, `.` and `..` left out, read as the walk goes. Output: the
    /// names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        use std::os::unix::ffi::OsStringExt;
        let listing = rustix::fs::Dir::read_from(&self.0)?;
        Ok(listing.filter_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name().to_bytes();
                (!matches!(name, b"." | b".."))
                    .then(|| Ok(std::ffi::OsString::from_vec(name.to_vec())))
            }
            Err(error) => Some(Err(error.into())),
        }))
    }

    /// Reads the link `name` in this folder without following it. Output: its target text,
    /// or the I/O error.
    pub(super) fn read_link(&self, name: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let target = rustix::fs::readlinkat(&self.0, name, Vec::new())?;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(
            target.into_bytes(),
        )))
    }

    /// Copies the regular file `name` in this folder, the file `seen` describes, to a new
    /// file at `copy`, never writing through a link there (RV-DS241-40). Inputs: the name,
    /// its metadata from the listing and the new path. Output: Ok, or the I/O error,
    /// `AlreadyExists` when anything holds `copy`.
    pub(super) fn copy_file(
        &self,
        name: &std::ffi::OsStr,
        seen: &fs::Metadata,
        copy: &Path,
    ) -> std::io::Result<()> {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let source = fs::File::from(rustix::fs::openat(&self.0, name, flags, Mode::empty())?);
        same_entry(&source, seen)?;
        copy_opened(&source, seen, copy)
    }
}

#[cfg(target_os = "windows")]
impl SourceFolder {
    /// Opens the folder `below` a copy's root by path once it is a real folder. Inputs: the
    /// root, the path below it and the listing's metadata, unused. Output: the folder, or
    /// the I/O error. Written, not tested.
    pub(super) fn open(root: &Path, below: &Path, _seen: &fs::Metadata) -> std::io::Result<Self> {
        let path = root.join(below);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("source folder replaced"));
        }
        Ok(Self(path))
    }

    /// The names in the folder. Output: the names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        Ok(fs::read_dir(&self.0)?.map(|entry| entry.map(|entry| entry.file_name())))
    }

    /// Reads the link `name` in this folder. Output: its target text, or the I/O error.
    pub(super) fn read_link(&self, name: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
        fs::read_link(self.0.join(name))
    }

    /// Copies the file `name` in this folder to `copy` once nothing holds that path, then
    /// gives it the listed permissions. Output: Ok, or the I/O error, `AlreadyExists` when
    /// anything holds `copy`. Written, not tested.
    pub(super) fn copy_file(
        &self,
        name: &std::ffi::OsStr,
        seen: &fs::Metadata,
        copy: &Path,
    ) -> std::io::Result<()> {
        if fs::symlink_metadata(copy).is_ok() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        fs::copy(self.0.join(name), copy)?;
        fs::set_permissions(copy, seen.permissions())
    }
}

/// Fails unless an opened entry is the one its listing's metadata describes: the same
/// device, inode and type. Inputs: the open entry and the listed metadata. Output: Ok, or
/// an I/O error naming the replaced entry.
#[cfg(unix)]
fn same_entry(opened: &fs::File, seen: &fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let now = opened.metadata()?;
    if now.dev() == seen.dev() && now.ino() == seen.ino() && now.file_type() == seen.file_type() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "source entry replaced during the copy",
        ))
    }
}

/// Creates the new file of a copy, failing when anything, a link included, holds its path.
/// Input: the path. Output: the file, open for writing, or the I/O error.
#[cfg(unix)]
fn new_file(copy: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(copy)
}

/// Copies an opened regular file to a new path. macOS: an APFS clone made only where
/// nothing holds the path yet, else the new file is created first and `fs::copy` runs from
/// one descriptor to the other through `/dev/fd/<n>`, which opens that same descriptor, so
/// the bytes, extended attributes, access list and permissions are written as before and no
/// path is resolved again. Inputs: the source, its listed metadata and the new path.
/// Output: Ok, or the I/O error.
#[cfg(target_os = "macos")]
fn copy_opened(source: &fs::File, seen: &fs::Metadata, copy: &Path) -> std::io::Result<()> {
    use rustix::io::Errno;
    use std::os::fd::AsRawFd;
    match rustix::fs::fclonefileat(
        source,
        rustix::fs::CWD,
        copy,
        rustix::fs::CloneFlags::empty(),
    ) {
        Ok(()) => return set_mode(copy, seen.permissions()),
        Err(Errno::NOTSUP | Errno::XDEV) => {}
        Err(error) => return Err(error.into()),
    }
    let written = new_file(copy)?;
    let handle = |file: &fs::File| PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
    fs::copy(handle(source), handle(&written)).map(drop)
}

/// Copies an opened regular file to a new path: its bytes and then its permissions, through
/// the new file's own descriptor. Inputs: the source, its listed metadata and the new path.
/// Output: Ok, or the I/O error.
#[cfg(all(unix, not(target_os = "macos")))]
fn copy_opened(source: &fs::File, seen: &fs::Metadata, copy: &Path) -> std::io::Result<()> {
    let mut written = new_file(copy)?;
    let mut reader = source;
    std::io::copy(&mut reader, &mut written)?;
    written.set_permissions(seen.permissions())
}

/// Opens a file a copy's check reads, only when it is the file `seen` describes. Unix:
/// never through a link at its path and never waiting on a FIFO. Inputs: the path and its
/// listed metadata. Output: the file, or the I/O error.
#[cfg(unix)]
pub(super) fn open_seen(path: &Path, seen: &fs::Metadata) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits() as i32)
        .open(path)?;
    same_entry(&file, seen)?;
    Ok(file)
}

/// Opens a file a copy's check reads. Windows: by path as before. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn open_seen(path: &Path, _seen: &fs::Metadata) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

/// Gives an entry the permissions `wanted`. macOS: never through a link at its path; a link
/// put there gets the mode itself. Input: the path and the permissions. Output: Ok, or the
/// I/O error.
#[cfg(target_os = "macos")]
pub(super) fn set_mode(path: &Path, wanted: fs::Permissions) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = rustix::fs::Mode::from_raw_mode(wanted.mode() as rustix::fs::RawMode);
    rustix::fs::chmodat(
        rustix::fs::CWD,
        path,
        mode,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )?;
    Ok(())
}

/// Gives an entry the permissions `wanted` by path. Other systems: Linux has no chmod that
/// refuses a link before kernel 6.6, and Windows keeps its read-only flag as before. Input:
/// the path and the permissions. Output: Ok, or the I/O error.
#[cfg(not(target_os = "macos"))]
pub(super) fn set_mode(path: &Path, wanted: fs::Permissions) -> std::io::Result<()> {
    fs::set_permissions(path, wanted)
}

/// Opens a file for appending, creating it, and refuses to follow a link
/// placed at its path. Input: the file path. Output: the open file.
#[cfg(unix)]
pub(super) fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Opens a file for appending, creating it, and refuses a link at its path by
/// opening the reparse point itself. Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        return Err(std::io::Error::other("exclude is a link"));
    }
    Ok(file)
}

/// Moves a file or folder to the macOS Trash through NSFileManager, which
/// needs no Finder automation permission. Input: the path. Output: Ok once
/// it is in the Trash.
#[cfg(target_os = "macos")]
pub(super) fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

/// Moves a file or folder to the desktop trash (freedesktop trash on Linux,
/// the Recycle Bin on Windows). Written, not tested.
#[cfg(not(target_os = "macos"))]
pub(super) fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    trash::delete(path)
}
