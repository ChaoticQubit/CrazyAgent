use super::{
    FOLDER, MANIFEST_FILE, Problem, RETIRED, STAGED, TREE_DEPTH_MAX, TREE_ENTRIES_MAX,
    USER_PLUGINS_MAX, UserFile, left_over, retired, retired_name,
};

use crate::Refused;
use crate::embedding::valid_name;
use crate::manifest::MANIFEST_BYTES_MAX;
use crate::store::refused;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const LEGACY_SUFFIX: &str = ".yaml";

/// The most entries read from one plugin folder before it is refused for holding more
/// than its plugin.toml.
const ENTRIES_MAX: usize = 4;

/// What one name in the plugins folder is: a plugin folder, or the `.yaml` file a
/// version before plugin folders wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Folder,
    Legacy,
}

fn unavailable() -> Refused {
    refused("app_plugin_folder_unavailable")
}

fn unsafe_entry() -> Problem {
    Problem::new("app_plugin_file_unsafe", None, None)
}

fn legacy() -> Problem {
    Problem::new("app_plugin_format_old", None, None)
}

fn open(root: &Path, create: bool) -> Result<Option<std::fs::File>, Refused> {
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
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(refused("app_plugin_folder_unsafe"));
    }
    Ok(Some(directory))
}

fn entry_of(file: &[u8]) -> Option<(&str, Entry)> {
    let name = std::str::from_utf8(file).ok()?;
    if let Some(stem) = name.strip_suffix(LEGACY_SUFFIX) {
        return valid_name(stem).then_some((stem, Entry::Legacy));
    }
    valid_name(name).then_some((name, Entry::Folder))
}

/// Reads a plugin folder's plugin.toml: the folder opened without following a link,
/// holding that one entry, the file a plain file read without following a link or
/// waiting on a FIFO, at most `MANIFEST_BYTES_MAX` + 1 bytes. Inputs: the folder and
/// whether it must be owned by this user. Output: the bytes, or the refusal.
fn read_dir(directory: std::fs::File, owned: bool) -> Result<Vec<u8>, Problem> {
    use std::io::Read;
    let uid = rustix::process::getuid().as_raw();
    let metadata = directory.metadata().map_err(|_| unsafe_entry())?;
    if !metadata.is_dir() || (owned && metadata.uid() != uid) {
        return Err(unsafe_entry());
    }
    let mut seen = 0;
    for entry in rustix::fs::Dir::read_from(&directory).map_err(|_| unsafe_entry())? {
        let entry = entry.map_err(|_| unsafe_entry())?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        seen += 1;
        if name != MANIFEST_FILE.as_bytes() || seen >= ENTRIES_MAX {
            return Err(Problem::new("plugin_folder_extra_entry", None, None));
        }
    }
    assert!(seen < ENTRIES_MAX);
    let file = std::fs::File::from(
        rustix::fs::openat(
            &directory,
            MANIFEST_FILE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOENT => Problem::new("plugin_manifest_invalid", Some(1), None),
            _ => unsafe_entry(),
        })?,
    );
    let metadata = file.metadata().map_err(|_| unsafe_entry())?;
    if !metadata.is_file() || (owned && metadata.uid() != uid) {
        return Err(unsafe_entry());
    }
    let mut bytes = Vec::new();
    file.take(MANIFEST_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_entry())?;
    assert!(bytes.len() <= MANIFEST_BYTES_MAX + 1);
    Ok(bytes)
}

fn read_in(directory: &std::fs::File, name: &str) -> Result<Vec<u8>, Problem> {
    let folder = rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| unsafe_entry())?;
    read_dir(std::fs::File::from(folder), true)
}

/// Reads a folder the owner picked to add: not a link itself, opened without following
/// one, holding its plugin.toml only. Input: the folder. Output: the
/// plugin.toml bytes, or the refusal.
pub(super) fn read_source(source: &Path) -> Result<Vec<u8>, Problem> {
    let metadata = std::fs::symlink_metadata(source).map_err(|_| unsafe_entry())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_entry());
    }
    let folder = rustix::fs::open(
        source,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| unsafe_entry())?;
    read_dir(std::fs::File::from(folder), false)
}

fn names(directory: &std::fs::File) -> Result<(BTreeMap<String, Entry>, bool), Refused> {
    let mut names = BTreeMap::new();
    let mut more = false;
    for entry in rustix::fs::Dir::read_from(directory).map_err(|_| unavailable())? {
        let entry = entry.map_err(|_| unavailable())?;
        let Some((name, kind)) = entry_of(entry.file_name().to_bytes()) else {
            continue;
        };
        let kept = names.entry(name.to_owned()).or_insert(kind);
        if kind == Entry::Folder {
            *kept = Entry::Folder;
        }
        if names.len() > USER_PLUGINS_MAX {
            names.pop_last();
            more = true;
        }
    }
    assert!(names.len() <= USER_PLUGINS_MAX);
    Ok((names, more))
}

/// The owner's plugins by name, at most `USER_PLUGINS_MAX`, each read without following
/// a link; a `.yaml` file an older version wrote is listed refused and kept. Input: the
/// engine root. Output: (name, bytes or refusal) pairs, whether more exist, or the
/// folder's refusal.
pub(super) fn entries(root: &Path) -> Result<(Vec<UserFile>, bool), Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok((Vec::new(), false));
    };
    let (names, more) = names(&directory)?;
    Ok((
        names
            .into_iter()
            .map(|(name, kind)| {
                let read = match kind {
                    Entry::Folder => read_in(&directory, &name),
                    Entry::Legacy => Err(legacy()),
                };
                (name, read)
            })
            .collect(),
        more,
    ))
}

/// Whether a plugin is among the first `USER_PLUGINS_MAX` by name, the ones `entries`
/// lists; one past them is refused so it never runs with an unkept port. Inputs: the
/// engine root and a valid plugin name. Output: the answer, or the folder's refusal.
pub(super) fn listed(root: &Path, name: &str) -> Result<bool, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(false);
    };
    let (names, _) = names(&directory)?;
    Ok(names.contains_key(name))
}

/// Whether a plugin of this name, once written, is among the first `USER_PLUGINS_MAX`
/// by name without dropping another from them: it is listed already, or the folder
/// holds fewer entries than the cap, old plugin files included. Inputs: the engine root
/// and a valid plugin name. Output: the answer, or the folder's refusal.
pub(super) fn fits(root: &Path, name: &str) -> Result<bool, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(true);
    };
    let (names, _) = names(&directory)?;
    Ok(names.contains_key(name) || names.len() < USER_PLUGINS_MAX)
}

fn exists(directory: &std::fs::File, name: &str) -> Result<bool, Refused> {
    match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

/// One user plugin. Inputs: the engine root and a valid plugin name. Output: none when
/// it is missing, else its plugin.toml bytes or the refusal (a left-over `.yaml` file
/// is `app_plugin_format_old`); or the folder's refusal.
pub(super) fn read(root: &Path, name: &str) -> Result<Option<Result<Vec<u8>, Problem>>, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(None);
    };
    if exists(&directory, name)? {
        return Ok(Some(read_in(&directory, name)));
    }
    if exists(&directory, &format!("{name}{LEGACY_SUFFIX}"))? {
        return Ok(Some(Err(legacy())));
    }
    Ok(None)
}

/// What one entry is, read without following a link. Inputs: its folder and name.
/// Output: its type, none when it is missing, or `app_plugin_folder_unavailable`.
fn kind(directory: &std::fs::File, name: &CStr) -> Result<Option<FileType>, Refused> {
    match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(_) => Err(unavailable()),
    }
}

fn unlink(directory: &std::fs::File, name: &CStr) -> Result<(), Refused> {
    match rustix::fs::unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err(unavailable()),
    }
}

/// Deletes one plugin folder and what it holds, never following a link: a link in it is
/// unlinked itself, so nothing outside the plugin is touched. Inputs: the plugins folder
/// and the entry name. Output: Ok once it is gone, or `app_plugin_folder_unavailable`
/// past `TREE_DEPTH_MAX` levels or `TREE_ENTRIES_MAX` entries in one folder.
fn remove_tree(directory: &std::fs::File, name: &str) -> Result<(), Refused> {
    let name = std::ffi::CString::new(name).map_err(|_| unavailable())?;
    remove_level(directory, &name, TREE_DEPTH_MAX)
}

fn remove_level(directory: &std::fs::File, name: &CStr, depth: usize) -> Result<(), Refused> {
    let folder = std::fs::File::from(
        rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?,
    );
    let mut inside = Vec::new();
    for entry in rustix::fs::Dir::read_from(&folder).map_err(|_| unavailable())? {
        let entry = entry.map_err(|_| unavailable())?;
        let file = entry.file_name();
        if file.to_bytes() == b"." || file.to_bytes() == b".." {
            continue;
        }
        inside.push(file.to_owned());
        if inside.len() > TREE_ENTRIES_MAX {
            return Err(unavailable());
        }
    }
    for file in &inside {
        match kind(&folder, file)? {
            Some(FileType::Directory) if depth == 0 => return Err(unavailable()),
            Some(FileType::Directory) => remove_level(&folder, file, depth - 1)?,
            Some(_) => unlink(&folder, file)?,
            None => {}
        }
    }
    rustix::fs::unlinkat(directory, name, AtFlags::REMOVEDIR).map_err(|_| unavailable())
}

/// Writes a user plugin as a staged 0700 folder holding a 0600 plugin.toml, renamed into
/// place, creating the 0700 plugins folder when missing. An older folder of that name is
/// moved aside first and moved back when the new one cannot take its place; once the
/// new one is in place it moves to a retired name with no plugin name in it and is
/// deleted, and one it cannot delete stays aside for the start sweep. Inputs: the engine
/// root, a valid name, the bytes and the random id source. Output: Ok with the folder
/// synced, or the folder's refusal with nothing staged left.
pub(super) fn write(
    root: &Path,
    name: &str,
    bytes: &[u8],
    new_ref: fn() -> Result<String, &'static str>,
) -> Result<(), Refused> {
    let directory = open(root, true)?.ok_or_else(unavailable)?;
    let id = new_ref().map_err(refused)?;
    let staged = format!("{STAGED}{id}");
    rustix::fs::mkdirat(&directory, staged.as_str(), Mode::from_raw_mode(0o700))
        .map_err(|_| unavailable())?;
    let written = (|| {
        let folder = std::fs::File::from(
            rustix::fs::openat(
                &directory,
                staged.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| unavailable())?,
        );
        let mut file = std::fs::File::from(
            rustix::fs::openat(
                &folder,
                MANIFEST_FILE,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| unavailable())?,
        );
        std::io::Write::write_all(&mut file, bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| folder.sync_all())
            .map_err(|_| unavailable())?;
        let retired = retired(name, &id);
        let replaced = exists(&directory, name)?;
        if replaced {
            rustix::fs::renameat(&directory, name, &directory, retired.as_str())
                .map_err(|_| unavailable())?;
        }
        if rustix::fs::renameat(&directory, staged.as_str(), &directory, name).is_err() {
            if replaced {
                let _ = rustix::fs::renameat(&directory, retired.as_str(), &directory, name);
            }
            return Err(unavailable());
        }
        if replaced {
            let aside = format!("{RETIRED}{id}");
            let nameless =
                rustix::fs::renameat(&directory, retired.as_str(), &directory, aside.as_str());
            let _ = remove_tree(&directory, if nameless.is_ok() { &aside } else { &retired });
        }
        Ok(())
    })();
    if written.is_err() {
        let _ = remove_tree(&directory, &staged);
    }
    written?;
    directory.sync_all().map_err(|_| unavailable())
}

/// Sweeps the staged and retired entries a crash between a write and its rename left, at
/// most `USER_PLUGINS_MAX` a start, in name order. Input: the engine root. Output: Ok
/// once each is restored or deleted or the folder is missing, else the first refusal
/// after every entry was tried.
pub(super) fn remove_staged(root: &Path) -> Result<(), Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(());
    };
    let mut left = Vec::new();
    for entry in rustix::fs::Dir::read_from(&directory).map_err(|_| unavailable())? {
        let entry = entry.map_err(|_| unavailable())?;
        if left_over(entry.file_name().to_bytes()) {
            left.push(entry.file_name().to_owned());
            if left.len() == USER_PLUGINS_MAX {
                break;
            }
        }
    }
    assert!(left.len() <= USER_PLUGINS_MAX);
    left.sort();
    let mut first = None;
    for file in &left {
        if let Err(refusal) = sweep(&directory, file) {
            first.get_or_insert(refusal);
        }
    }
    directory.sync_all().map_err(|_| unavailable())?;
    first.map_or(Ok(()), Err)
}

/// Restores or deletes one staged or retired entry: a retired folder goes back under its
/// plugin name when that name is missing, any other folder is deleted, and a file or
/// link is unlinked. Inputs: the plugins folder and the entry. Output: Ok, or the
/// refusal.
fn sweep(directory: &std::fs::File, file: &CStr) -> Result<(), Refused> {
    match kind(directory, file)? {
        None => Ok(()),
        Some(FileType::Directory) => {
            if let Some(name) = file.to_str().ok().and_then(retired_name)
                && !exists(directory, name)?
            {
                return rustix::fs::renameat(directory, file, directory, name)
                    .map_err(|_| unavailable());
            }
            remove_level(directory, file, TREE_DEPTH_MAX)
        }
        Some(_) => unlink(directory, file),
    }
}

/// Deletes a user plugin's folder and any `.yaml` file an older version left under its
/// name. The folder first moves to a retired name with no plugin name in it, so one it
/// cannot delete is never listed or restored again and stays for the start sweep; an
/// entry that is not a folder, such as a link, is unlinked itself without following it.
/// Inputs: the engine root, a valid name and the random id source. Output: whether
/// anything was deleted, or the folder's refusal.
pub(super) fn remove(
    root: &Path,
    name: &str,
    new_ref: fn() -> Result<String, &'static str>,
) -> Result<bool, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(false);
    };
    let kind = match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Some(rustix::fs::FileType::from_raw_mode(stat.st_mode)),
        Err(rustix::io::Errno::NOENT) => None,
        Err(_) => return Err(unavailable()),
    };
    match kind {
        Some(rustix::fs::FileType::Directory) => {
            let aside = format!("{RETIRED}{}", new_ref().map_err(refused)?);
            rustix::fs::renameat(&directory, name, &directory, aside.as_str())
                .map_err(|_| unavailable())?;
            let _ = remove_tree(&directory, &aside);
        }
        Some(_) => match rustix::fs::unlinkat(&directory, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return Err(unavailable()),
        },
        None => {}
    }
    let folder = kind.is_some();
    let legacy = match rustix::fs::unlinkat(
        &directory,
        format!("{name}{LEGACY_SUFFIX}").as_str(),
        AtFlags::empty(),
    ) {
        Ok(()) => true,
        Err(rustix::io::Errno::NOENT) => false,
        Err(_) => return Err(unavailable()),
    };
    Ok(folder || legacy)
}
