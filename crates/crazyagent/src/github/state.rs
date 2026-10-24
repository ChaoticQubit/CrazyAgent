use super::{CLIENT_ID, GitHubMethod};
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const CONFIG: &str = "github.toml";
const STATE: &str = "github.json";
#[cfg(unix)]
const FILE_BYTES_MAX: u64 = 4096;
const CLIENT_ID_BYTES_MAX: usize = 64;

/// What the engine remembers about the owner's GitHub sign-in: the method, the login name
/// and the credential store reference. It never holds the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Saved {
    pub(super) method: GitHubMethod,
    pub(super) login: String,
    pub(super) credential: CredentialRef,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Refuses GitHub config and sign-in files where their Unix owner and mode cannot be
/// checked. Takes the path, unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
fn read_private(_path: &Path) -> Result<Option<Vec<u8>>, EngineError> {
    Err(failure("github_state_unavailable"))
}

#[cfg(unix)]
fn read_private(path: &Path) -> Result<Option<Vec<u8>>, EngineError> {
    let unsafe_file = || failure("github_config_unsafe");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unsafe_file()),
    };
    let metadata = file.metadata().map_err(|_| unsafe_file())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > FILE_BYTES_MAX
    {
        return Err(unsafe_file());
    }
    let mut bytes = Vec::new();
    file.take(FILE_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_file())?;
    if bytes.len() as u64 > FILE_BYTES_MAX {
        return Err(unsafe_file());
    }
    Ok(Some(bytes))
}

/// The OAuth App client id: the compiled default unless `<engine root>/github.toml` sets
/// `client_id`. Input: the engine root. Output: the id, or
/// `github_client_id_not_configured` when the override is empty.
pub(super) fn client_id(root: &Path) -> Result<String, EngineError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        client_id: String,
    }
    let Some(bytes) = read_private(&root.join(CONFIG))? else {
        return Ok(CLIENT_ID.to_owned());
    };
    let text = String::from_utf8(bytes).map_err(|_| failure("github_config_invalid"))?;
    let config: Config = toml::from_str(&text).map_err(|_| failure("github_config_invalid"))?;
    let id = config.client_id.trim();
    if id.is_empty() {
        return Err(failure("github_client_id_not_configured"));
    }
    if id.len() > CLIENT_ID_BYTES_MAX
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(failure("github_config_invalid"));
    }
    Ok(id.to_owned())
}

/// Reads the saved sign-in. Output: None when the owner has not signed in.
pub(super) fn load(root: &Path) -> Result<Option<Saved>, EngineError> {
    let Some(bytes) = read_private(&root.join(STATE))? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| failure("github_state_invalid"))
}

/// Forgets the saved sign-in. Output: Ok also when nothing was saved.
pub(super) fn forget(root: &Path) -> Result<(), EngineError> {
    match std::fs::remove_file(root.join(STATE)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(failure("github_state_unavailable"))
        }
        _ => Ok(()),
    }
}

/// Saves the sign-in as a private 0600 file, replaced in one rename.
/// Inputs: the engine root and what to remember.
#[cfg(unix)]
pub(super) fn save(root: &Path, saved: &Saved) -> Result<(), EngineError> {
    let unavailable = || failure("github_state_unavailable");
    let bytes = serde_json::to_vec(saved).map_err(|_| unavailable())?;
    assert!(bytes.len() as u64 <= FILE_BYTES_MAX);
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|_| unavailable())?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = root.join(format!(".{STATE}.{suffix}"));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, root.join(STATE))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(unavailable());
    }
    Ok(())
}

/// Refuses to save the sign-in where no owner-only 0600 no-follow file can be made.
/// Takes the engine root and sign-in, both unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
pub(super) fn save(_root: &Path, _saved: &Saved) -> Result<(), EngineError> {
    Err(failure("github_state_unavailable"))
}
