#[cfg(windows)]
use crazyagent_win_sandbox::private;
use kernel_processes::finding::PROJECT_ROOTS_MAX;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The side file in a data folder that keeps its last good read of the set-up projects.
pub(super) const NAME: &str = "places-last-good";

/// The side file's first line, naming its format and version.
const MAGIC: &[u8] = b"crazyagent places-last-good 1\n";

/// The most bytes a side file may hold: a longer list is not written, and a longer file is
/// not read.
pub(super) const FILE_BYTES_MAX: u64 = 16 << 20;

/// A data folder's stored last good read of the set-up projects
/// ([`kernel_processes::finding::FindingHooks::last_good_load`]): the side file, read only
/// when it is a plain private file of this user within the bound, opened without following
/// a link, in the format [`encode`] writes. Anything else gives none, as no file does.
/// Input: the data folder. Output: the folders, or none.
pub(super) fn load(data_root: &Path) -> Option<Vec<PathBuf>> {
    let roots = decode(&read_private(&data_root.join(NAME))?)?;
    assert!(roots.len() <= PROJECT_ROOTS_MAX);
    assert!(roots.iter().all(|root| root.is_absolute()));
    Some(roots)
}

/// Stores a data folder's good read of the set-up projects
/// ([`kernel_processes::finding::FindingHooks::last_good_store`]) as a new private file
/// renamed over the side file, only when the file does not already hold that list; no file
/// counts as an empty list, so an empty read writes nothing. It never makes the data folder,
/// and a list past the bound or any write failure leaves the old file and is logged.
/// Inputs: the data folder and the list.
pub(super) fn store(data_root: &Path, roots: &[PathBuf]) {
    let Some(bytes) = encode(roots) else {
        eprintln!("engine_places_last_good_unsaved reason=too_large");
        return;
    };
    let held = read_private(&data_root.join(NAME));
    if held.as_deref().unwrap_or(MAGIC) == bytes.as_slice() {
        return;
    }
    if write(data_root, &bytes).is_err() {
        eprintln!("engine_places_last_good_unsaved reason=write_failed");
    }
}

/// Reads a private file: a plain file this user owns with no group or other bits and one
/// name on Unix, judged private on Windows, at most [`FILE_BYTES_MAX`] bytes, opened
/// without following a link or waiting on a pipe. Input: the path. Output: the bytes, or
/// none for no file or any other file.
fn read_private(path: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .ok()?;
    #[cfg(windows)]
    let file = private::read_only(path).ok()?;
    let metadata = file.metadata().ok()?;
    #[cfg(unix)]
    let owned = metadata.is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o077 == 0
        && metadata.nlink() == 1;
    #[cfg(windows)]
    let owned = metadata.is_file();
    if !owned || metadata.len() > FILE_BYTES_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(FILE_BYTES_MAX + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= FILE_BYTES_MAX).then_some(bytes)
}

/// The side file's bytes: [`MAGIC`], then each folder's bytes followed by a NUL, raw on Unix
/// and UTF-8 on Windows. A folder that holds a NUL or on Windows is not valid Unicode is
/// left out, which for that folder is no last good read. Input: the
/// folders. Output: the bytes, or none past [`FILE_BYTES_MAX`].
fn encode(roots: &[PathBuf]) -> Option<Vec<u8>> {
    assert!(roots.iter().all(|root| root.is_absolute()));
    let mut bytes = MAGIC.to_vec();
    for root in roots {
        let Some(part) = bytes_of(root).filter(|part| !part.contains(&0)) else {
            continue;
        };
        bytes.extend_from_slice(part);
        bytes.push(0);
        if bytes.len() as u64 > FILE_BYTES_MAX {
            return None;
        }
    }
    Some(bytes)
}

/// The folders in side file bytes ([`encode`]). Input: the bytes. Output: the folders, or
/// none for a wrong first line, a missing last NUL, more than [`PROJECT_ROOTS_MAX`] folders,
/// or a folder that is empty, not absolute or on Windows not UTF-8.
fn decode(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let body = bytes.strip_prefix(MAGIC)?;
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut roots = Vec::new();
    for part in body.strip_suffix(&[0])?.split(|byte| *byte == 0) {
        if roots.len() == PROJECT_ROOTS_MAX {
            return None;
        }
        let root = path_of(part)?;
        if !root.is_absolute() {
            return None;
        }
        roots.push(root);
    }
    Some(roots)
}

/// A folder's bytes in the side file. Input: the folder. Output: its raw bytes.
#[cfg(unix)]
fn bytes_of(root: &Path) -> Option<&[u8]> {
    use std::os::unix::ffi::OsStrExt;
    Some(root.as_os_str().as_bytes())
}

/// A folder's bytes in the side file. Input: the folder. Output: its UTF-8 bytes, or none
/// when it is not valid Unicode.
#[cfg(windows)]
fn bytes_of(root: &Path) -> Option<&[u8]> {
    root.to_str().map(str::as_bytes)
}

/// The folder one side file entry names. Input: the entry's bytes. Output: the folder.
#[cfg(unix)]
fn path_of(part: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(part)))
}

/// The folder one side file entry names. Input: the entry's bytes. Output: the folder, or
/// none when they are not UTF-8.
#[cfg(windows)]
fn path_of(part: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(part).ok().map(PathBuf::from)
}

/// Writes the side file as a new private file in the data folder (0600 and created new
/// without following a link on Unix, the owner-only list on Windows), synced, then renamed
/// over the name, so a reader sees the old file or the new one and a link at the name is
/// replaced, never followed. The temporary file is removed on failure. Inputs: the data
/// folder and the bytes. Output: Ok, or the error.
fn write(data_root: &Path, bytes: &[u8]) -> std::io::Result<()> {
    assert!(bytes.len() as u64 <= FILE_BYTES_MAX);
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|_| std::io::Error::other("no random bytes"))?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = data_root.join(format!(".{NAME}.{suffix}"));
    let written = (|| {
        #[cfg(unix)]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&temporary)?;
        #[cfg(windows)]
        let mut file = private::file(&temporary, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, data_root.join(NAME))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}
