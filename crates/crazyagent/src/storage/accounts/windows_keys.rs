use super::*;

/// Deletes one account key file from the engine root's W1 private key folder on Windows, for
/// the start sweep of keys a committed removal left behind; the folder is held while the
/// file goes. Inputs: the engine root and the key's credential reference. Output: Ok when the
/// file is gone, including when it or the folder was already missing;
/// `account_key_store_unsafe` for a folder W1's check refuses.
#[cfg(windows)]
pub(crate) fn remove_key_file(root: &Path, name: &str) -> Result<(), EngineError> {
    remove_key_in(root, ACCOUNT_KEYS, name)
}

/// Deletes one key file from one of the engine root's private key folders, the folder held
/// while the file goes. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and the key
/// name. Output: Ok when the file is gone, including when it or the folder was already
/// missing; `account_key_store_unsafe` for a folder the private check refuses.
pub(crate) fn remove_key_in(root: &Path, folder: &str, name: &str) -> Result<(), EngineError> {
    let Some((_held, folder)) = key_folder(root, folder, name, false)? else {
        return Ok(());
    };
    match std::fs::remove_file(folder.join(name)) {
        Ok(()) => Ok(()),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(error("account_key_store_unavailable")),
    }
}

/// Opens the engine root's key folder on Windows as a W1 secret store folder for one key
/// name, refusing a name that is not a plain credential reference or holds a colon, which
/// Windows reads as a stream of another file; a trailing dot or space and a device name are
/// never a reference (`protocol::single_component`). Inputs: the engine root, a folder from
/// [`KEY_FOLDERS`], the key name and whether to make a missing folder, with the store's
/// label. Output: the held folder and its path, none when it does not exist and `create` is
/// false, `account_key_reference_invalid`, or `account_key_store_unsafe` for a folder the
/// private check refuses.
fn key_folder(
    root: &Path,
    folder: &str,
    name: &str,
    create: bool,
) -> Result<Option<(std::fs::File, std::path::PathBuf)>, EngineError> {
    if CredentialRef::new(name).is_err() || name.starts_with('.') || name.contains(':') {
        return Err(error("account_key_reference_invalid"));
    }
    assert!(crate::protocol::single_component(name));
    store_folder(root, folder, create)
}

/// Opens one of the engine root's key folders as a secret store folder with the store's
/// label, made when missing and asked. Inputs: the engine root, a folder from
/// [`KEY_FOLDERS`] and whether to make it. Output: the held folder and its path, none when
/// it does not exist and `create` is false, or `account_key_store_unsafe` for a folder the
/// private check refuses.
fn store_folder(
    root: &Path,
    folder: &str,
    create: bool,
) -> Result<Option<(std::fs::File, std::path::PathBuf)>, EngineError> {
    assert!(KEY_FOLDERS.contains(&folder), "a known key folder");
    let path = root.join(folder);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound && !create => {
            return Ok(None);
        }
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(error("account_key_store_unavailable")),
    }
    match crazyagent_win_sandbox::private::store_dir(
        &path,
        crazyagent_win_sandbox::private::Store::Secret,
    ) {
        Ok(held) => Ok(Some((held, path))),
        Err(problem) if problem.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(error("account_key_store_unsafe"))
        }
        Err(_) => Err(error("account_key_store_unavailable")),
    }
}

/// Reads one saved account key on Windows so the engine can hand it to its key relay, never
/// to an agent (DEC-141 PA-4), as a W1 private file read without delete sharing, so it is not
/// renamed or replaced while read. Inputs: the engine root and the key's credential
/// reference. Output: the key, none when it was never saved, or `account_key_store_unsafe`
/// for a key file or folder W1's check refuses.
#[cfg(windows)]
pub(crate) fn read_key_file(
    root: &Path,
    credential: &CredentialRef,
) -> Result<Option<kernel_security::secret::SecretString>, EngineError> {
    read_key_in(root, ACCOUNT_KEYS, credential)
}

/// Reads one saved key from one of the engine root's private key folders into engine memory
/// only. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and the key's reference.
/// Output: the key, none when it was never saved, or the refusals of [`read_key_file`].
pub(crate) fn read_key_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
) -> Result<Option<kernel_security::secret::SecretString>, EngineError> {
    let Some(file) = open_key_in(root, folder, credential)? else {
        return Ok(None);
    };
    let len = file.metadata().map_err(error)?.len();
    key_text(file, len).map(Some)
}

/// Saves an account key on Windows as a new W1 secret file in the engine root's key folder,
/// made with its store label when missing, the twin of the Unix 0600 `O_EXCL` write.
/// Inputs: the engine root, the key's credential reference and the key, 1 to
/// `KEY_BYTES_MAX` bytes. Output: the folder synced; `account_key_store_unavailable` when a
/// key is already saved under that reference or the write fails, which leaves no file.
#[cfg(windows)]
pub(crate) fn store_key_file(
    root: &Path,
    credential: &CredentialRef,
    key: &[u8],
) -> Result<(), EngineError> {
    let (_held, folder) = key_folder(root, ACCOUNT_KEYS, credential.as_str(), true)?
        .ok_or_else(|| error("account_key_store_unavailable"))?;
    write_secret(&folder.join(credential.as_str()), key)?;
    crazyagent_win_sandbox::private::sync_folder(&folder)
        .map_err(|_| error("account_key_store_unavailable"))
}

/// Swaps the saved key of one credential for a new one on Windows: the new key is written to
/// a staged W1 secret file in the held key folder, then renamed over the old file, so the
/// file never holds a partial key and keeps the secret file's own list and label. A reader
/// holding the old key shares no delete, so the rename then fails. Inputs: the engine root,
/// the credential reference and the new key, 1 to `KEY_BYTES_MAX` bytes. Output: the folder
/// synced; on `account_key_store_unavailable` the old key stays and nothing is staged.
#[cfg(windows)]
pub(crate) fn replace_key_file(
    root: &Path,
    credential: &CredentialRef,
    key: &[u8],
) -> Result<(), EngineError> {
    replace_in(root, ACCOUNT_KEYS, credential, key)
}

/// Saves or replaces one key in one of the engine root's private key folders, made with its
/// store label when missing. The key is checked to be 1 to `KEY_BYTES_MAX` bytes and wiped
/// on every path. Inputs: the engine root, a folder from [`KEY_FOLDERS`], the key's
/// reference and the key. Output: Ok with the folder synced; on
/// `account_key_store_unavailable` the old key stays and nothing is staged.
pub(crate) fn replace_key_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
    mut key: Vec<u8>,
) -> Result<(), EngineError> {
    let saved = if key.is_empty() || key.len() as u64 > KEY_BYTES_MAX {
        Err(error("account_key_store_unavailable"))
    } else {
        replace_in(root, folder, credential, &key)
    };
    kernel_security::secret::wipe(&mut key);
    saved
}

/// The disk work of [`replace_key_file`] and [`replace_key_in`]: a staged secret file in the
/// held folder renamed over the key. Inputs: the engine root, a folder from
/// [`KEY_FOLDERS`], the reference and the key, 1 to `KEY_BYTES_MAX` bytes. Output: Ok with
/// the folder synced, or `account_key_store_unavailable` with the old key kept.
fn replace_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
    key: &[u8],
) -> Result<(), EngineError> {
    let (_held, folder) = key_folder(root, folder, credential.as_str(), true)?
        .ok_or_else(|| error("account_key_store_unavailable"))?;
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| error("account_key_store_unavailable"))?;
    let staged = folder.join(format!("{STAGED_KEY}{:016x}", u64::from_le_bytes(nonce)));
    write_secret(&staged, key)?;
    if std::fs::rename(&staged, folder.join(credential.as_str())).is_err() {
        let _ = std::fs::remove_file(&staged);
        return Err(error("account_key_store_unavailable"));
    }
    crazyagent_win_sandbox::private::sync_folder(&folder)
        .map_err(|_| error("account_key_store_unavailable"))
}

/// Writes a key into a new W1 secret file, which carries the medium no-read-up label from
/// creation, syncs it and closes it; on a failed write the file is closed, then removed.
/// Inputs: a path in the held key folder that must not exist, and the key. Output: Ok, or
/// `account_key_store_unavailable`.
#[cfg(windows)]
fn write_secret(path: &Path, key: &[u8]) -> Result<(), EngineError> {
    assert!(!key.is_empty());
    assert!(key.len() as u64 <= KEY_BYTES_MAX);
    let mut file = crazyagent_win_sandbox::private::secret_file(path)
        .map_err(|_| error("account_key_store_unavailable"))?;
    let written = std::io::Write::write_all(&mut file, key)
        .and_then(|()| file.sync_all())
        .and_then(|()| file.metadata());
    match written {
        Ok(metadata) => {
            assert_eq!(metadata.len(), key.len() as u64);
            Ok(())
        }
        Err(_) => {
            drop(file);
            let _ = std::fs::remove_file(path);
            Err(error("account_key_store_unavailable"))
        }
    }
}

/// Opens one saved account key on Windows as a W1 private file read without delete sharing,
/// so it is not renamed or replaced while read, for the key relay and the owner transport.
/// Inputs: the engine root and the key's credential reference. Output: the open file, none
/// when it was never saved, or `account_key_store_unsafe` for a key file or folder W1
/// refuses or a file longer than `KEY_BYTES_MAX`.
#[cfg(windows)]
pub(crate) fn open_key_file(
    root: &Path,
    credential: &CredentialRef,
) -> Result<Option<std::fs::File>, EngineError> {
    open_key_in(root, ACCOUNT_KEYS, credential)
}

/// Opens one saved key in one of the engine root's private key folders as [`open_key_file`]
/// does. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and the reference. Output:
/// the open file, none when it was never saved, or its refusals.
fn open_key_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
) -> Result<Option<std::fs::File>, EngineError> {
    let Some((_held, folder)) = key_folder(root, folder, credential.as_str(), false)? else {
        return Ok(None);
    };
    let file = match crazyagent_win_sandbox::private::read_only(&folder.join(credential.as_str())) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(problem) if problem.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(error("account_key_store_unsafe"));
        }
        Err(_) => return Err(error("account_key_store_unavailable")),
    };
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || metadata.len() > KEY_BYTES_MAX {
        return Err(error("account_key_store_unsafe"));
    }
    Ok(Some(file))
}

/// Whether a key is saved in one of the engine root's private key folders, without reading
/// it, after the folder passes the private check. Inputs: the engine root, a folder from
/// [`KEY_FOLDERS`] and the key's reference. Output: true for a plain file, false when it or
/// the folder is missing or it is a link, a folder or any reparse point, or the folder's
/// refusal.
pub(crate) fn key_saved_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
) -> Result<bool, EngineError> {
    use std::os::windows::fs::MetadataExt;
    let Some((_held, folder)) = key_folder(root, folder, credential.as_str(), false)? else {
        return Ok(false);
    };
    match std::fs::symlink_metadata(folder.join(credential.as_str())) {
        Ok(metadata) => Ok(
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        ),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(error("account_key_store_unavailable")),
    }
}

/// The Windows file attribute of a reparse point, such as a symbolic link or a junction.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The plain key names in one held key folder, at most [`KEY_SWEEP_MAX`] entries read, a
/// staged copy or a name that is no plain reference left out. Input: the folder path.
/// Output: the names, or `account_key_store_unavailable`.
fn key_names(folder: &Path) -> Result<Vec<String>, EngineError> {
    let mut names = Vec::new();
    let mut read = 0;
    for entry in std::fs::read_dir(folder).map_err(|_| error("account_key_store_unavailable"))? {
        if read == KEY_SWEEP_MAX {
            break;
        }
        read += 1;
        let entry = entry.map_err(|_| error("account_key_store_unavailable"))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with('.') && CredentialRef::new(&name).is_ok() {
            names.push(name);
        }
    }
    assert!(names.len() <= read && read <= KEY_SWEEP_MAX);
    Ok(names)
}

/// Deletes the staged key copies a crash between a replace's write and its rename left in
/// one of the engine root's private key folders, at most [`STAGED_SWEEP_MAX`]. The engine
/// runs it at start, before any replace can stage one. Inputs: the engine root and a folder
/// from [`KEY_FOLDERS`]. Output: Ok once those found are gone or the folder is missing, the
/// folder's refusal, or `account_key_store_unavailable`.
pub(crate) fn remove_staged_in(root: &Path, folder: &str) -> Result<(), EngineError> {
    let Some((_held, folder)) = store_folder(root, folder, false)? else {
        return Ok(());
    };
    let mut staged = Vec::new();
    for entry in std::fs::read_dir(&folder).map_err(|_| error("account_key_store_unavailable"))? {
        let entry = entry.map_err(|_| error("account_key_store_unavailable"))?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(STAGED_KEY.as_bytes())
        {
            staged.push(entry.file_name());
            if staged.len() == STAGED_SWEEP_MAX {
                break;
            }
        }
    }
    assert!(staged.len() <= STAGED_SWEEP_MAX);
    for name in &staged {
        match std::fs::remove_file(folder.join(name)) {
            Ok(()) => {}
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error("account_key_store_unavailable")),
        }
    }
    crazyagent_win_sandbox::private::sync_folder(&folder)
        .map_err(|_| error("account_key_store_unavailable"))
}

/// Deletes every key in one of the engine root's private key folders that `kept` does not
/// keep, and the staged copies. It reads at most [`KEY_SWEEP_MAX`] entries and leaves a name
/// that is no plain reference alone. A key whose removal fails stays and does not stop the
/// others; a failed test stops the sweep before any key is deleted. Inputs: the engine root,
/// a folder from [`KEY_FOLDERS`] and the test by key name. Output: how many keys were
/// deleted, or the folder's refusal, the test's failure or the first failed removal.
pub(crate) fn remove_unkept_in(
    root: &Path,
    folder: &str,
    kept: impl Fn(&str) -> Result<bool, EngineError>,
) -> Result<usize, EngineError> {
    let mut failed = remove_staged_in(root, folder).err();
    let Some((held, path)) = store_folder(root, folder, false)? else {
        return failed.map_or(Ok(0), Err);
    };
    let mut unkept = Vec::new();
    for name in key_names(&path)? {
        if !kept(name.as_str())? {
            unkept.push(name);
        }
    }
    drop(held);
    let mut removed = 0;
    for name in &unkept {
        match remove_key_in(root, folder, name) {
            Ok(()) => removed += 1,
            Err(cause) => {
                failed.get_or_insert(cause);
            }
        }
    }
    assert!(removed <= unkept.len());
    failed.map_or(Ok(removed), Err)
}
