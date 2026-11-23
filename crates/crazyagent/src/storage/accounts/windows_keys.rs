use super::*;

/// Deletes one account key file from the engine root's W1 private key folder on Windows, for
/// the start sweep of keys a committed removal left behind; the folder is held while the
/// file goes. Inputs: the engine root and the key's credential reference. Output: Ok when the
/// file is gone, including when it or the folder was already missing;
/// `account_key_store_unsafe` for a folder W1's check refuses.
#[cfg(windows)]
pub(crate) fn remove_key_file(root: &Path, name: &str) -> Result<(), EngineError> {
    let Some((_held, folder)) = key_folder(root, name, false)? else {
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
/// never a reference (`protocol::single_component`). Inputs: the engine root, the key name
/// and whether to make a missing folder, with the store's label. Output: the held folder and
/// its path, none when it does not exist and `create` is false,
/// `account_key_reference_invalid`, or `account_key_store_unsafe` for a folder W1 refuses.
#[cfg(windows)]
fn key_folder(
    root: &Path,
    name: &str,
    create: bool,
) -> Result<Option<(std::fs::File, std::path::PathBuf)>, EngineError> {
    if CredentialRef::new(name).is_err() || name.starts_with('.') || name.contains(':') {
        return Err(error("account_key_reference_invalid"));
    }
    assert!(crate::protocol::single_component(name));
    let path = root.join(ACCOUNT_KEYS);
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
    let Some(file) = open_key_file(root, credential)? else {
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
    let (_held, folder) = key_folder(root, credential.as_str(), true)?
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
    let (_held, folder) = key_folder(root, credential.as_str(), true)?
        .ok_or_else(|| error("account_key_store_unavailable"))?;
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| error("account_key_store_unavailable"))?;
    let staged = folder.join(format!(".replace-{:016x}", u64::from_le_bytes(nonce)));
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
    let Some((_held, folder)) = key_folder(root, credential.as_str(), false)? else {
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
