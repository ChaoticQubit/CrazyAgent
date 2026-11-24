use crate::Refused;
use crate::registry::{FOLDER, MANIFEST_FILE};
use crate::store::refused;
use crazyagent_win_sandbox::private;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const DELETE: u32 = 0x0001_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// A folder held open by its handle, which never shares delete, so it is neither renamed
/// nor replaced while held, with the path its entries are opened under.
pub(super) struct Dir {
    handle: File,
    path: PathBuf,
}

/// What one entry is, read without following a link: a folder, or anything else, a link,
/// a junction or any other reparse point included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Folder,
    Other,
}

/// Opens the private plugins folder, creating it first when asked, and refuses one that is
/// a link or a junction, or not private to this user. Inputs: the engine root and whether
/// to create. Output: the held folder, none when missing, or the refusal.
pub(super) fn open_plugins(root: &Path, create: bool) -> Result<Option<Dir>, Refused> {
    let path = root.join(FOLDER);
    let opened = if create {
        private::dir(&path)
    } else {
        private::open_dir(&path)
    };
    match opened {
        Ok(handle) => Ok(Some(Dir { handle, path })),
        Err(error) if !create && error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            Err(refused("app_plugin_folder_unsafe"))
        }
        Err(_) => Err(refused("app_plugin_folder_unavailable")),
    }
}

/// Hands each entry name of a held folder to `visit` until `visit` answers false. Inputs:
/// the folder and the visitor. Output: Ok, or Err when the folder or an entry cannot be
/// read.
pub(super) fn each(directory: &Dir, mut visit: impl FnMut(&OsStr) -> bool) -> Result<(), ()> {
    for entry in std::fs::read_dir(&directory.path).map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        if !visit(&entry.file_name()) {
            break;
        }
    }
    Ok(())
}

/// What one entry of a held folder is, read without following a link. Inputs: the folder
/// and the name. Output: its kind, none when it is missing, or Err.
pub(super) fn kind(directory: &Dir, name: &OsStr) -> Result<Option<Kind>, ()> {
    match std::fs::symlink_metadata(directory.path.join(name)) {
        Ok(metadata)
            if metadata.is_dir()
                && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 =>
        {
            Ok(Some(Kind::Folder))
        }
        Ok(_) => Ok(Some(Kind::Other)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// Opens one path as itself, never following a link, sharing read and write only.
/// Inputs: the path and the access. Output: the handle, or the error.
fn opened(path: &Path, access: u32) -> std::io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Whether an opened object is no reparse point, of the wanted shape, and, when asked,
/// private to this user as every store judges what it opens: one name, a local volume, a
/// trusted owner and no other account that may open it. Inputs: the handle, whether a
/// folder is wanted and whether it must be owned. Output: the answer, false when it cannot
/// be read.
fn plain(object: &File, folder: bool, owned: bool) -> bool {
    let Ok(metadata) = object.metadata() else {
        return false;
    };
    if metadata.is_dir() != folder || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return false;
    }
    !owned || private::check(object).is_ok()
}

fn held(path: PathBuf, owned: bool) -> Result<Dir, ()> {
    let handle = opened(
        &path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL,
    )
    .map_err(|_| ())?;
    if !plain(&handle, true, owned) {
        return Err(());
    }
    Ok(Dir { handle, path })
}

/// Opens one folder in a held folder as itself, owned by a trusted owner on a local
/// volume when asked. Inputs: the folder, the name and whether it must be owned. Output:
/// the held folder, or Err.
pub(super) fn open_folder(directory: &Dir, name: &OsStr, owned: bool) -> Result<Dir, ()> {
    held(directory.path.join(name), owned)
}

/// Opens a folder the owner picked as itself, refusing a link or a junction. Input: the
/// path. Output: the held folder, or Err.
pub(super) fn open_picked(source: &Path) -> Result<Dir, ()> {
    held(source.to_path_buf(), false)
}

/// Opens a held plugin folder's plugin.toml for reading as itself, a plain file with a
/// trusted owner on a local volume when asked. Inputs: the folder and whether it must be
/// owned. Output: the file, none when it is missing, or Err.
pub(super) fn open_manifest(folder: &Dir, owned: bool) -> Result<Option<File>, ()> {
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(folder.path.join(MANIFEST_FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !plain(&file, false, owned) {
        return Err(());
    }
    Ok(Some(file))
}

/// Deletes one entry through its own handle, a link or junction itself and never its
/// target, the handle opened, marked and closed in this call. An entry the delete refuses
/// for its read-only attribute has that attribute cleared through the same handle and is
/// deleted again, unless it has another name, which may sit outside the plugins folder and
/// shares the attribute: that entry is left as it is and Err returned, for the sweep to
/// retry. Inputs: the held folder and the name. Output: whether it was there, or Err.
pub(super) fn unlink(directory: &Dir, name: &OsStr) -> Result<bool, ()> {
    let entry = match opened(
        &directory.path.join(name),
        DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
    ) {
        Ok(entry) => entry,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    if private::delete(&entry).is_err() {
        let read_only = entry
            .metadata()
            .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_READONLY != 0);
        let one_name = private::facts(&entry).is_ok_and(|facts| facts.links == 1);
        if !read_only || !one_name {
            return Err(());
        }
        private::clear_read_only(&entry).map_err(|_| ())?;
        private::delete(&entry).map_err(|_| ())?;
    }
    drop(entry);
    Ok(true)
}

/// Deletes one empty folder through its own handle. Inputs: the held folder holding it
/// and its name. Output: Ok, or Err, a missing folder included.
pub(super) fn remove_dir(directory: &Dir, name: &OsStr) -> Result<(), ()> {
    if unlink(directory, name)? {
        Ok(())
    } else {
        Err(())
    }
}

/// Renames one entry inside a held folder through the entry's own handle, never over an
/// existing name; an entry another program holds without sharing delete is refused.
/// Inputs: the folder, the old and the new name. Output: Ok, or Err.
pub(super) fn rename(directory: &Dir, from: &OsStr, to: &OsStr) -> Result<(), ()> {
    let entry =
        opened(&directory.path.join(from), DELETE | FILE_READ_ATTRIBUTES).map_err(|_| ())?;
    private::rename(&entry, &directory.handle, to, private::Mode::NoReplace).map_err(|_| ())
}

/// Makes one new private folder in a held folder, holding nothing once it returns. Inputs:
/// the folder and the name. Output: Ok, or Err when the name is taken or the folder
/// cannot be made.
pub(super) fn make_folder(directory: &Dir, name: &OsStr) -> Result<(), ()> {
    private::new_dir(&directory.path.join(name))
        .map(drop)
        .map_err(|_| ())
}

/// Creates a new private plugin.toml in one folder of a held folder, writes the bytes and
/// syncs the file and its folder, holding nothing once it returns. Inputs: the held
/// folder, the folder name and the bytes. Output: Ok, or Err.
pub(super) fn write_manifest(directory: &Dir, name: &OsStr, bytes: &[u8]) -> Result<(), ()> {
    let folder = directory.path.join(name);
    let mut file = private::file(&folder.join(MANIFEST_FILE), true).map_err(|_| ())?;
    std::io::Write::write_all(&mut file, bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ())?;
    drop(file);
    private::sync_folder(&folder).map_err(|_| ())
}

/// Writes a held folder's entries to disk. Input: the folder. Output: Ok, or Err.
pub(super) fn sync(directory: &Dir) -> Result<(), ()> {
    private::sync_folder(&directory.path).map_err(|_| ())
}
