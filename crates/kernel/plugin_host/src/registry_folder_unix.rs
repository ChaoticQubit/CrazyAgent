use crate::Refused;
use crate::registry::{FOLDER, MANIFEST_FILE};
use crate::store::refused;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// A folder held open by its descriptor, so every entry in it is reached through that
/// descriptor and never by a path walk.
pub(super) struct Dir(std::fs::File);

/// What one entry is, read without following a link: a folder, or anything else, a link
/// included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Folder,
    Other,
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Opens the plugins folder without following a link, creating it 0700 first when asked,
/// and refuses one that is not a folder owned by this user with mode 0700. Inputs: the
/// engine root and whether to create. Output: the held folder, none when missing, or the
/// refusal.
pub(super) fn open_plugins(root: &Path, create: bool) -> Result<Option<Dir>, Refused> {
    let unavailable = || refused("app_plugin_folder_unavailable");
    if create {
        match rustix::fs::mkdir(root.join(FOLDER), Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(unavailable()),
        }
    }
    let directory = match rustix::fs::open(
        root.join(FOLDER),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
            return Err(refused("app_plugin_folder_unsafe"));
        }
        Err(_) => return Err(unavailable()),
    };
    let metadata = directory.metadata().map_err(|_| unavailable())?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o7777 != 0o700 {
        return Err(refused("app_plugin_folder_unsafe"));
    }
    Ok(Some(Dir(directory)))
}

/// Hands each entry name of a held folder to `visit`, `.` and `..` left out, until
/// `visit` answers false. Inputs: the folder and the visitor. Output: Ok, or Err when the
/// folder or an entry cannot be read.
pub(super) fn each(directory: &Dir, mut visit: impl FnMut(&OsStr) -> bool) -> Result<(), ()> {
    for entry in rustix::fs::Dir::read_from(&directory.0).map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if !visit(OsStr::from_bytes(name)) {
            break;
        }
    }
    Ok(())
}

/// What one entry of a held folder is, read without following a link. Inputs: the folder
/// and the name. Output: its kind, none when it is missing, or Err.
pub(super) fn kind(directory: &Dir, name: &OsStr) -> Result<Option<Kind>, ()> {
    match rustix::fs::statat(&directory.0, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => {
            Ok(Some(Kind::Folder))
        }
        Ok(_) => Ok(Some(Kind::Other)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(_) => Err(()),
    }
}

fn held(folder: std::fs::File, owned: bool) -> Result<Dir, ()> {
    let metadata = folder.metadata().map_err(|_| ())?;
    if !metadata.is_dir() || (owned && metadata.uid() != uid()) {
        return Err(());
    }
    Ok(Dir(folder))
}

/// Opens one folder in a held folder without following a link, owned by this user when
/// asked. Inputs: the folder, the name and whether it must be owned. Output: the held
/// folder, or Err.
pub(super) fn open_folder(directory: &Dir, name: &OsStr, owned: bool) -> Result<Dir, ()> {
    let folder = rustix::fs::openat(
        &directory.0,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ())?;
    held(std::fs::File::from(folder), owned)
}

/// Opens a folder the owner picked without following a link, refusing one that is a link
/// itself. Input: the path. Output: the held folder, or Err.
pub(super) fn open_picked(source: &Path) -> Result<Dir, ()> {
    let metadata = std::fs::symlink_metadata(source).map_err(|_| ())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(());
    }
    let folder = rustix::fs::open(
        source,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ())?;
    held(std::fs::File::from(folder), false)
}

/// Opens a held plugin folder's plugin.toml for reading without following a link or
/// waiting on a FIFO, a plain file owned by this user when asked. Inputs: the folder and
/// whether it must be owned. Output: the file, none when it is missing, or Err.
pub(super) fn open_manifest(folder: &Dir, owned: bool) -> Result<Option<std::fs::File>, ()> {
    let file = match rustix::fs::openat(
        &folder.0,
        MANIFEST_FILE,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file() || (owned && metadata.uid() != uid()) {
        return Err(());
    }
    Ok(Some(file))
}

/// Unlinks one entry that is no folder, a link itself and never its target. Inputs: the
/// held folder and the name. Output: whether it was there, or Err.
pub(super) fn unlink(directory: &Dir, name: &OsStr) -> Result<bool, ()> {
    match rustix::fs::unlinkat(&directory.0, name, AtFlags::empty()) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err(()),
    }
}

/// Deletes one empty folder. Inputs: the held folder holding it and its name. Output: Ok,
/// or Err.
pub(super) fn remove_dir(directory: &Dir, name: &OsStr) -> Result<(), ()> {
    rustix::fs::unlinkat(&directory.0, name, AtFlags::REMOVEDIR).map_err(|_| ())
}

/// Renames one entry inside a held folder. Inputs: the folder, the old and the new name.
/// Output: Ok, or Err.
pub(super) fn rename(directory: &Dir, from: &OsStr, to: &OsStr) -> Result<(), ()> {
    rustix::fs::renameat(&directory.0, from, &directory.0, to).map_err(|_| ())
}

/// Makes one new 0700 folder in a held folder. Inputs: the folder and the name. Output:
/// Ok, or Err when the name is taken or the folder cannot be made.
pub(super) fn make_folder(directory: &Dir, name: &OsStr) -> Result<(), ()> {
    rustix::fs::mkdirat(&directory.0, name, Mode::from_raw_mode(0o700)).map_err(|_| ())
}

/// Creates a new 0600 plugin.toml in one folder of a held folder, writes the bytes and
/// syncs the file and its folder, holding nothing once it returns. Inputs: the held
/// folder, the folder name and the bytes. Output: Ok, or Err.
pub(super) fn write_manifest(directory: &Dir, name: &OsStr, bytes: &[u8]) -> Result<(), ()> {
    let folder = std::fs::File::from(
        rustix::fs::openat(
            &directory.0,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ())?,
    );
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            &folder,
            MANIFEST_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| ())?,
    );
    std::io::Write::write_all(&mut file, bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| folder.sync_all())
        .map_err(|_| ())
}

/// Writes a held folder's entries to disk. Input: the folder. Output: Ok, or Err.
pub(super) fn sync(directory: &Dir) -> Result<(), ()> {
    directory.0.sync_all().map_err(|_| ())
}
