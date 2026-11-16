use super::job::GitHubPublishJob;
use super::rules::RepoRecord;
use super::{CLIENT_ID, GitHubMethod};
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const CONFIG: &str = "github.toml";
const STATE: &str = "github.json";
const RECORDS: &str = "github-repos";
const JOBS: &str = "github-publish";
const JOB_BYTES_MAX: u64 = 8 << 20;
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

/// Refuses a private file where its Unix owner and mode cannot be checked. Takes the path
/// and bound, both unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
fn read_private_within(_path: &Path, _max: u64) -> Result<Option<Vec<u8>>, EngineError> {
    Err(failure("github_state_unavailable"))
}

/// Reads a private file of at most `FILE_BYTES_MAX`, as [`read_private_within`] does.
/// Input: the path. Output: the bytes, None when there is no file, or
/// `github_config_unsafe`.
#[cfg(unix)]
fn read_private(path: &Path) -> Result<Option<Vec<u8>>, EngineError> {
    read_private_within(path, FILE_BYTES_MAX)
}

/// Reads a private file: a plain file the engine's user owns, open to no one else, with
/// one link and at most `max` bytes, opened without following a link. Inputs: the path
/// and the bound. Output: the bytes, None when there is no file, or `github_config_unsafe`.
#[cfg(unix)]
fn read_private_within(path: &Path, max: u64) -> Result<Option<Vec<u8>>, EngineError> {
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
        || metadata.len() > max
    {
        return Err(unsafe_file());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_file())?;
    if bytes.len() as u64 > max {
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
    let bytes = serde_json::to_vec(saved).map_err(|_| failure("github_state_unavailable"))?;
    write_private(root, STATE, &bytes)
}

/// Writes a private file of at most `FILE_BYTES_MAX`, as [`write_private_within`] does.
/// Inputs: the folder, the file name and the bytes. Output: Ok, or
/// `github_state_unavailable`.
#[cfg(unix)]
fn write_private(folder: &Path, name: &str, bytes: &[u8]) -> Result<(), EngineError> {
    write_private_within(folder, name, bytes, FILE_BYTES_MAX)
}

/// Writes a private 0600 file in a private folder, replaced in one rename, so a reader
/// sees the old file or the new one. Inputs: the folder, the file name, the bytes and
/// their bound. Output: Ok, or `github_state_unavailable`.
#[cfg(unix)]
fn write_private_within(
    folder: &Path,
    name: &str,
    bytes: &[u8],
    max: u64,
) -> Result<(), EngineError> {
    let unavailable = || failure("github_state_unavailable");
    assert!(bytes.len() as u64 <= max);
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(|_| unavailable())?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = folder.join(format!(".{name}.{suffix}"));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, folder.join(name))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(unavailable());
    }
    Ok(())
}

/// The folder and file that hold what CrazyAgent remembers about one repository: the file
/// is named by the SHA-256 of the lowercase `owner/name`, since GitHub matches names in
/// any case, so every name maps to one plain file name. Inputs: the engine root and
/// `owner/name`. Output: the folder and the file name, or `github_repo_name_invalid`.
fn record_file(root: &Path, full_name: &str) -> Result<(PathBuf, String), EngineError> {
    super::clone::full_name(full_name)?;
    let digest = Sha256::digest(full_name.to_ascii_lowercase().as_bytes());
    let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((root.join(RECORDS), format!("{name}.json")))
}

/// Reads what CrazyAgent remembers about one repository. Inputs: the engine root and
/// `owner/name`. Output: the record, empty when none is saved, or
/// `github_state_invalid` for a file that is not a record of CrazyAgent's rulesets.
pub(super) fn load_record(root: &Path, full_name: &str) -> Result<RepoRecord, EngineError> {
    let (folder, name) = record_file(root, full_name)?;
    let Some(bytes) = read_private(&folder.join(name))? else {
        return Ok(RepoRecord::default());
    };
    let record: RepoRecord =
        serde_json::from_slice(&bytes).map_err(|_| failure("github_state_invalid"))?;
    if !record.valid() {
        return Err(failure("github_state_invalid"));
    }
    Ok(record)
}

/// Saves what CrazyAgent remembers about one repository as a private file in the engine
/// root's private `github-repos` folder, made 0700 when missing. Inputs: the engine root,
/// `owner/name` and the record. Output: Ok, or `github_config_unsafe` for a folder that is
/// a link or open to others, or `github_state_unavailable`.
#[cfg(unix)]
pub(super) fn save_record(
    root: &Path,
    full_name: &str,
    record: &RepoRecord,
) -> Result<(), EngineError> {
    let (folder, name) = record_file(root, full_name)?;
    private_folder(&folder)?;
    let bytes = serde_json::to_vec(record).map_err(|_| failure("github_state_unavailable"))?;
    write_private(&folder, &name, &bytes)
}

/// Makes a private 0700 folder in the engine root when it is missing and checks the one
/// there: a plain folder the engine's user owns, open to no one else. Input: the folder.
/// Output: Ok, or `github_config_unsafe`, `github_state_unavailable`.
#[cfg(unix)]
fn private_folder(folder: &Path) -> Result<(), EngineError> {
    match std::fs::DirBuilder::new().mode(0o700).create(folder) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(failure("github_state_unavailable"));
        }
        _ => {}
    }
    let metadata =
        std::fs::symlink_metadata(folder).map_err(|_| failure("github_config_unsafe"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(failure("github_config_unsafe"));
    }
    Ok(())
}

/// The folder and file that hold one project's publish job: the file is named by the
/// SHA-256 of the project folder's path. Inputs: the engine root and the project folder.
/// Output: the folder and the file name.
fn job_file(root: &Path, project: &Path) -> (PathBuf, String) {
    let digest = Sha256::digest(project.as_os_str().as_encoded_bytes());
    let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    (root.join(JOBS), format!("{name}.json"))
}

/// Reads one project's publish job. Inputs: the engine root and the project folder.
/// Output: the job, None when none is saved, or `github_state_invalid` for a file that
/// is not this project's job, `github_config_unsafe`.
pub(super) fn load_job(
    root: &Path,
    project: &Path,
) -> Result<Option<GitHubPublishJob>, EngineError> {
    let (folder, name) = job_file(root, project);
    let Some(bytes) = read_private_within(&folder.join(name), JOB_BYTES_MAX)? else {
        return Ok(None);
    };
    let job: GitHubPublishJob =
        serde_json::from_slice(&bytes).map_err(|_| failure("github_state_invalid"))?;
    if job.project != project {
        return Err(failure("github_state_invalid"));
    }
    Ok(Some(job))
}

/// Saves one project's publish job as a private file in the engine root's private
/// `github-publish` folder, replaced in one rename. Inputs: the engine root and the job.
/// Output: Ok, or `github_config_unsafe`, `github_state_unavailable`.
#[cfg(unix)]
pub(super) fn save_job(root: &Path, job: &GitHubPublishJob) -> Result<(), EngineError> {
    let (folder, name) = job_file(root, &job.project);
    private_folder(&folder)?;
    let bytes = serde_json::to_vec(job).map_err(|_| failure("github_state_unavailable"))?;
    if bytes.len() as u64 > JOB_BYTES_MAX {
        return Err(failure("github_state_unavailable"));
    }
    write_private_within(&folder, &name, &bytes, JOB_BYTES_MAX)
}

/// Refuses to save a publish job where no owner-only file can be made. Takes the engine
/// root and the job, both unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
pub(super) fn save_job(_root: &Path, _job: &GitHubPublishJob) -> Result<(), EngineError> {
    Err(failure("github_state_unavailable"))
}

/// Removes one project's publish job, as Discard does. Inputs: the engine root and the
/// project folder. Output: Ok, also when no job is saved, or `github_state_unavailable`.
#[cfg(unix)]
pub(super) fn remove_job(root: &Path, project: &Path) -> Result<(), EngineError> {
    let (folder, name) = job_file(root, project);
    match std::fs::remove_file(folder.join(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failure("github_state_unavailable")),
    }
}

/// Refuses to remove a publish job where none can be saved. Takes the engine root and the
/// project folder, both unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
pub(super) fn remove_job(_root: &Path, _project: &Path) -> Result<(), EngineError> {
    Err(failure("github_state_unavailable"))
}

/// Refuses to save a repository record where no owner-only file can be made. Takes the
/// engine root, name and record, all unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
pub(super) fn save_record(
    _root: &Path,
    _full_name: &str,
    _record: &RepoRecord,
) -> Result<(), EngineError> {
    Err(failure("github_state_unavailable"))
}

/// Refuses to save the sign-in where no owner-only 0600 no-follow file can be made.
/// Takes the engine root and sign-in, both unused. Returns `github_state_unavailable`.
#[cfg(not(unix))]
pub(super) fn save(_root: &Path, _saved: &Saved) -> Result<(), EngineError> {
    Err(failure("github_state_unavailable"))
}
