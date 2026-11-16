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
/// following it; Windows keeps file and folder links apart. Written, not tested.
#[cfg(target_os = "windows")]
fn copy_link(original: &Path, target: PathBuf, copy: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if fs::symlink_metadata(original)?.file_type().is_symlink_dir() {
        std::os::windows::fs::symlink_dir(target, copy)
    } else {
        std::os::windows::fs::symlink_file(target, copy)
    }
}

/// What a copy, a refill or the walk that makes a tree writable met when an entry it listed
/// was no longer the one it opened: replaced, or swapped for a link (RV-DS241-41,
/// DS-INTAKE). It rides inside an `io::Error`, so the refusal can name it.
#[derive(Debug)]
pub(super) struct EntrySwapped;

impl std::fmt::Display for EntrySwapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an entry was replaced while the walk ran")
    }
}

impl std::error::Error for EntrySwapped {}

/// The error for an entry replaced while a walk ran. Output: an `io::Error` holding
/// [`EntrySwapped`].
pub(super) fn swapped() -> std::io::Error {
    std::io::Error::other(EntrySwapped)
}

/// Whether an error is [`swapped`]'s. Input: the error. Output: the answer.
pub(super) fn is_swapped(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<EntrySwapped>())
}

/// The error for a no-follow open of a listed entry: a link, or something other than a
/// folder, met where a part was listed as a folder (ELOOP, ENOTDIR) is [`swapped`].
/// Input: the open's error. Output: the error to answer.
#[cfg(unix)]
fn met_link(error: rustix::io::Errno) -> std::io::Error {
    use rustix::io::Errno;
    if matches!(error, Errno::LOOP | Errno::NOTDIR) {
        swapped()
    } else {
        error.into()
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
        let folder = crate::platform::open_beneath(root, below, flags).map_err(met_link)?;
        let folder = fs::File::from(folder);
        same_entry(&folder, seen)?;
        Ok(Self(folder))
    }

    /// The names in the folder, `.` and `..` left out, read as the walk goes. Output: the
    /// names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        listed(&self.0)
    }

    /// Reads the link `name` in this folder without following it. Output: its target text,
    /// or the I/O error, [`swapped`] when `name` is no longer a link.
    pub(super) fn read_link(&self, name: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let target = rustix::fs::readlinkat(&self.0, name, Vec::new()).map_err(|error| {
            if error == rustix::io::Errno::INVAL {
                swapped()
            } else {
                error.into()
            }
        })?;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(
            target.into_bytes(),
        )))
    }

    /// Copies the regular file `name` in this folder, the file `seen` describes, to a new
    /// file of the same name in `into`, never writing through a link there (RV-DS241-40).
    /// Inputs: the name, its metadata from the listing and the folder to write in. Output:
    /// Ok, or the I/O error, `AlreadyExists` when anything holds the name in `into`.
    pub(super) fn copy_file(
        &self,
        name: &std::ffi::OsStr,
        seen: &fs::Metadata,
        into: &TargetFolder,
    ) -> std::io::Result<()> {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let source = rustix::fs::openat(&self.0, name, flags, Mode::empty()).map_err(met_link)?;
        let source = fs::File::from(source);
        same_entry(&source, seen)?;
        copy_opened(&source, seen, &into.0, name)
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
            return Err(swapped());
        }
        Ok(Self(path))
    }

    /// The names in the folder. Output: the names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        listed(&self.0)
    }

    /// Reads the link `name` in this folder. Output: its target text, or the I/O error.
    pub(super) fn read_link(&self, name: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
        fs::read_link(self.0.join(name))
    }

    /// Copies the file `name` in this folder to the same name in `into` once nothing holds
    /// that path, then gives it the listed permissions. Output: Ok, or the I/O error,
    /// `AlreadyExists` when anything holds the name in `into`. Written, not tested.
    pub(super) fn copy_file(
        &self,
        name: &std::ffi::OsStr,
        seen: &fs::Metadata,
        into: &TargetFolder,
    ) -> std::io::Result<()> {
        let copy = into.0.join(name);
        if fs::symlink_metadata(&copy).is_ok() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        fs::copy(self.0.join(name), &copy)?;
        fs::set_permissions(&copy, seen.permissions())
    }
}

/// The names in an open folder, `.` and `..` left out, read as the walk goes. Input: the
/// folder. Output: the names, or the I/O error.
#[cfg(unix)]
fn listed(
    folder: &fs::File,
) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
    use std::os::unix::ffi::OsStringExt;
    let listing = rustix::fs::Dir::read_from(folder)?;
    Ok(listing.filter_map(|entry| match entry {
        Ok(entry) => {
            let name = entry.file_name().to_bytes();
            (!matches!(name, b"." | b"..")).then(|| Ok(std::ffi::OsString::from_vec(name.to_vec())))
        }
        Err(error) => Some(Err(error.into())),
    }))
}

/// The names in a folder. Windows: read by path. Input: the folder. Output: the names, or
/// the I/O error. Written, not tested.
#[cfg(target_os = "windows")]
fn listed(
    folder: &Path,
) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
    Ok(fs::read_dir(folder)?.map(|entry| entry.map(|entry| entry.file_name())))
}

/// What a folder of a tree being written holds at one name, a link not followed: nothing,
/// a real folder, or anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Held {
    Nothing,
    Folder,
    Other,
}

/// A folder a copy or a refill writes in, or the walk that makes a tree writable opens up,
/// opened from the tree's root one plain name at a time with no link followed in any part,
/// so a folder swapped for a link is refused, never written or walked through (DS-INTAKE).
/// Unix: the open folder, each entry made or changed relative to it.
#[cfg(unix)]
pub(super) struct TargetFolder(fs::File);

/// A folder a copy or a refill writes in, or the walk that makes a tree writable opens up.
/// Windows: its path, checked to be a real folder when opened, each entry made or changed
/// by path as before. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) struct TargetFolder(PathBuf);

#[cfg(unix)]
impl TargetFolder {
    /// Opens the folder `below` a tree's root. Inputs: the root and the plain names below it
    /// (empty for the root itself). Output: the folder, or the I/O error, [`swapped`] when a
    /// part is a link or not a folder now.
    pub(super) fn open(root: &Path, below: &Path) -> std::io::Result<Self> {
        use rustix::fs::OFlags;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let folder = crate::platform::open_beneath(root, below, flags).map_err(met_link)?;
        Ok(Self(fs::File::from(folder)))
    }

    /// The names in the folder, `.` and `..` left out. Output: the names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        listed(&self.0)
    }

    /// What holds `name` in this folder, a link not followed. Input: the name. Output: the
    /// answer, or the I/O error.
    pub(super) fn holds(&self, name: &std::ffi::OsStr) -> std::io::Result<Held> {
        use rustix::fs::{AtFlags, FileType};
        match rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(found) if FileType::from_raw_mode(found.st_mode) == FileType::Directory => {
                Ok(Held::Folder)
            }
            Ok(_) => Ok(Held::Other),
            Err(rustix::io::Errno::NOENT) => Ok(Held::Nothing),
            Err(error) => Err(error.into()),
        }
    }

    /// Makes the folder `name` in this folder, private to the owner until it gets its real
    /// permissions. Input: the name. Output: Ok, or the I/O error, `AlreadyExists` when
    /// anything holds the name.
    pub(super) fn make_folder(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        rustix::fs::mkdirat(&self.0, name, rustix::fs::Mode::RWXU)?;
        Ok(())
    }

    /// Makes `name` in this folder the same link as `_original`, never following it. Inputs:
    /// the name, the original link and its target text. Output: Ok, or the I/O error,
    /// `AlreadyExists` when anything holds the name.
    pub(super) fn make_link(
        &self,
        name: &std::ffi::OsStr,
        _original: &Path,
        target: PathBuf,
    ) -> std::io::Result<()> {
        rustix::fs::symlinkat(target, &self.0, name)?;
        Ok(())
    }

    /// The folder's own metadata, read through its handle. Output: it, or the I/O error.
    pub(super) fn metadata(&self) -> std::io::Result<fs::Metadata> {
        self.0.metadata()
    }

    /// Gives the folder itself the permissions `wanted`, through its handle. Input: the
    /// permissions. Output: Ok, or the I/O error.
    pub(super) fn set_mode(&self, wanted: fs::Permissions) -> std::io::Result<()> {
        self.0.set_permissions(wanted)
    }
}

#[cfg(target_os = "linux")]
impl TargetFolder {
    /// Adds owner read, write and search to the folder `name` in this folder, its other bits
    /// kept. Linux has no chmod that refuses a link before kernel 6.6, so the entry is pinned
    /// first ([`pin`]). Input: the name. Output: true once a folder was opened up, false for
    /// a link or anything else there now, or the I/O error.
    pub(super) fn open_up(&self, name: &std::ffi::OsStr) -> std::io::Result<bool> {
        use rustix::fs::{FileType, Mode};
        let (pinned, mode) = pin(&self.0, name)?;
        if FileType::from_raw_mode(mode) != FileType::Directory {
            return Ok(false);
        }
        chmod_pinned(&pinned, Mode::from_raw_mode(mode) | Mode::RWXU)?;
        Ok(true)
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
impl TargetFolder {
    /// Adds owner read, write and search to the folder `name` in this folder, its other bits
    /// kept, through `chmodat` with `AT_SYMLINK_NOFOLLOW` on this folder's handle; a link put
    /// there meanwhile gets the mode itself. Input: the name. Output: true once a folder was
    /// opened up, false for a link or anything else there, or the I/O error.
    pub(super) fn open_up(&self, name: &std::ffi::OsStr) -> std::io::Result<bool> {
        use rustix::fs::{AtFlags, FileType, Mode};
        let found = rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(found.st_mode) != FileType::Directory {
            return Ok(false);
        }
        let mode = Mode::from_raw_mode(found.st_mode) | Mode::RWXU;
        rustix::fs::chmodat(&self.0, name, mode, AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(true)
    }
}

#[cfg(target_os = "windows")]
impl TargetFolder {
    /// Opens the folder `below` a tree's root by path once it is a real folder. Inputs: the
    /// root and the path below it. Output: the folder, or the I/O error, [`swapped`] for a
    /// link or anything else. Written, not tested.
    pub(super) fn open(root: &Path, below: &Path) -> std::io::Result<Self> {
        let path = root.join(below);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(swapped());
        }
        Ok(Self(path))
    }

    /// The names in the folder. Output: the names, or the I/O error.
    pub(super) fn names(
        &self,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<std::ffi::OsString>>> {
        listed(&self.0)
    }

    /// What holds `name` in this folder, a link not followed. Input: the name. Output: the
    /// answer, or the I/O error.
    pub(super) fn holds(&self, name: &std::ffi::OsStr) -> std::io::Result<Held> {
        match fs::symlink_metadata(self.0.join(name)) {
            Ok(found) if found.is_dir() && !found.file_type().is_symlink() => Ok(Held::Folder),
            Ok(_) => Ok(Held::Other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Held::Nothing),
            Err(error) => Err(error),
        }
    }

    /// Makes the folder `name` in this folder, writable until it gets its real permissions.
    /// Input: the name. Output: Ok, or the I/O error.
    pub(super) fn make_folder(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        let path = self.0.join(name);
        fs::create_dir(&path)?;
        private_folder(&path)
    }

    /// Makes `name` in this folder the same link as `original`. Inputs: the name, the
    /// original link and its target text. Output: Ok, or the I/O error.
    pub(super) fn make_link(
        &self,
        name: &std::ffi::OsStr,
        original: &Path,
        target: PathBuf,
    ) -> std::io::Result<()> {
        copy_link(original, target, &self.0.join(name))
    }

    /// Clears the read-only flag of the folder `name` in this folder. Input: the name.
    /// Output: true once a folder was opened up, false for a link or anything else there, or
    /// the I/O error.
    pub(super) fn open_up(&self, name: &std::ffi::OsStr) -> std::io::Result<bool> {
        let path = self.0.join(name);
        let found = fs::symlink_metadata(&path)?;
        if !found.is_dir() || found.file_type().is_symlink() {
            return Ok(false);
        }
        private_folder(&path).map(|()| true)
    }

    /// The folder's own metadata. Output: it, or the I/O error.
    pub(super) fn metadata(&self) -> std::io::Result<fs::Metadata> {
        fs::symlink_metadata(&self.0)
    }

    /// Gives the folder itself the permissions `wanted`. Input: the permissions. Output: Ok,
    /// or the I/O error.
    pub(super) fn set_mode(&self, wanted: fs::Permissions) -> std::io::Result<()> {
        fs::set_permissions(&self.0, wanted)
    }
}

/// Fails unless an opened entry is the one its listing's metadata describes: the same
/// device, inode and type. Inputs: the open entry and the listed metadata. Output: Ok, or
/// [`swapped`].
#[cfg(unix)]
fn same_entry(opened: &fs::File, seen: &fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let now = opened.metadata()?;
    if now.dev() == seen.dev() && now.ino() == seen.ino() && now.file_type() == seen.file_type() {
        Ok(())
    } else {
        Err(swapped())
    }
}

/// Creates the new file `name` of a copy in the open folder `into`, failing when anything, a
/// link included, holds the name. Inputs: the folder and the name. Output: the file, open
/// for writing, or the I/O error.
#[cfg(unix)]
fn new_file(into: &fs::File, name: &std::ffi::OsStr) -> std::io::Result<fs::File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
    let mode = Mode::RUSR | Mode::WUSR;
    let written = rustix::fs::openat(into, name, flags | OFlags::CLOEXEC, mode)?;
    Ok(fs::File::from(written))
}

/// Copies an opened regular file to a new file `name` in the open folder `into`. macOS: an
/// APFS clone made only where nothing holds the name yet, else the new file is created first
/// and `fs::copy` runs from one descriptor to the other through `/dev/fd/<n>`, so the bytes,
/// extended attributes, access list and permissions are written as before and no path is
/// resolved again. Inputs: the source, its listed metadata, the folder and the name.
/// Output: Ok, or the I/O error.
#[cfg(target_os = "macos")]
fn copy_opened(
    source: &fs::File,
    seen: &fs::Metadata,
    into: &fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use rustix::io::Errno;
    use std::os::fd::AsRawFd;
    match rustix::fs::fclonefileat(source, into, name, rustix::fs::CloneFlags::empty()) {
        Ok(()) => return set_mode_at(into, name, &seen.permissions()),
        Err(Errno::NOTSUP | Errno::XDEV) => {}
        Err(error) => return Err(error.into()),
    }
    let written = new_file(into, name)?;
    let handle = |file: &fs::File| PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
    fs::copy(handle(source), handle(&written)).map(drop)
}

/// Copies an opened regular file to a new file `name` in the open folder `into`: its bytes
/// and then its permissions, through the new file's own descriptor. Inputs: the source, its
/// listed metadata, the folder and the name. Output: Ok, or the I/O error.
#[cfg(all(unix, not(target_os = "macos")))]
fn copy_opened(
    source: &fs::File,
    seen: &fs::Metadata,
    into: &fs::File,
    name: &std::ffi::OsStr,
) -> std::io::Result<()> {
    let mut written = new_file(into, name)?;
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

/// Gives an entry the permissions `wanted`, never through a link at its path. Linux has no
/// chmod that refuses a link before kernel 6.6, so the entry is pinned first ([`pin`]) and a
/// link there is [`swapped`]. Input: the path and the permissions. Output: Ok, or the I/O
/// error.
#[cfg(target_os = "linux")]
fn set_mode(path: &Path, wanted: fs::Permissions) -> std::io::Result<()> {
    use rustix::fs::{FileType, Mode};
    use std::os::unix::fs::PermissionsExt;
    let (pinned, mode) = pin(rustix::fs::CWD, path)?;
    if FileType::from_raw_mode(mode) == FileType::Symlink {
        return Err(swapped());
    }
    chmod_pinned(&pinned, Mode::from_raw_mode(wanted.mode()))
}

/// Gives an entry the permissions `wanted`, never through a link at its path; a link put
/// there gets the mode itself ([`set_mode_at`]). Input: the path and the permissions.
/// Output: Ok, or the I/O error.
#[cfg(all(unix, not(target_os = "linux")))]
fn set_mode(path: &Path, wanted: fs::Permissions) -> std::io::Result<()> {
    set_mode_at(rustix::fs::CWD, path, &wanted)
}

/// Gives the entry `name` in the open folder `dir` the permissions `wanted` through `chmodat`
/// with `AT_SYMLINK_NOFOLLOW`, so a link put there gets the mode itself. Inputs: the folder,
/// the name and the permissions. Output: Ok, or the I/O error.
#[cfg(all(unix, not(target_os = "linux")))]
fn set_mode_at<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
    wanted: &fs::Permissions,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = rustix::fs::Mode::from_raw_mode(wanted.mode() as rustix::fs::RawMode);
    rustix::fs::chmodat(dir, name, mode, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(())
}

/// Pins the entry `name` in the open folder `dir` with `O_PATH`, no link followed, so its
/// mode can change through [`chmod_pinned`] and never through a link. Inputs: the folder and
/// the name. Output: the pinned entry and its mode, type bits included, or the I/O error.
#[cfg(target_os = "linux")]
fn pin<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
) -> std::io::Result<(std::os::fd::OwnedFd, rustix::fs::RawMode)> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let pinned = rustix::fs::openat(dir, name, flags, Mode::empty())?;
    let mode = rustix::fs::fstat(&pinned)?.st_mode;
    Ok((pinned, mode))
}

/// Changes the mode of an entry [`pin`] pinned through `/proc/self/fd/<n>`, which names that
/// entry and no path. Inputs: the pinned entry and the mode. Output: Ok, or the I/O error.
#[cfg(target_os = "linux")]
fn chmod_pinned(pinned: &std::os::fd::OwnedFd, mode: rustix::fs::Mode) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    rustix::fs::chmod(
        format!("/proc/self/fd/{}", pinned.as_raw_fd()).as_str(),
        mode,
    )?;
    Ok(())
}

/// Opens a file for appending, creating it, and refuses to follow a link
/// placed at its path or to wait on a FIFO there. Input: the file path.
/// Output: the open file.
#[cfg(unix)]
pub(super) fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(flags.bits() as i32)
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
