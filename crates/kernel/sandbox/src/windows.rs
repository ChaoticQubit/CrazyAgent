use crate::error::{SandboxError, invalid};
use crate::paths::clean_absolute;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The boundary helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-win-boundary.exe";

/// Why a Windows launch is refused when the boundary helper is missing, in plain words.
pub const APPCONTAINER_MISSING: &str = "Windows agents start through crazyagent-win-boundary.exe next to the engine, which is missing, so no agent starts unconfined";

/// Why a Seatbelt profile is refused on Windows, in plain words.
pub const SEATBELT_MISSING: &str =
    "Windows has no Seatbelt; agents start through the AppContainer boundary";

/// The folder in the engine's data root where the boundary helper records the access entries
/// it grants each agent's container, so a later, narrower profile can take them back.
pub const GRANTS_FOLDER: &str = "boundary-grants";

/// The start of every agent's AppContainer name.
pub(crate) const CONTAINER_PREFIX: &str = "crazyagent.";
const CONTAINER_HASH_BYTES: usize = 16;

/// The AppContainer name for one agent of one project: [`CONTAINER_PREFIX`] and 32 hex digits
/// of the SHA-256 of the project folder and the agent's home, so each agent home is reachable
/// by its own container only, also for a release whose folders are already gone. Inputs: the
/// project folder and the agent's home. Output: the container name.
pub fn container_for(project_root: &Path, agent_home: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_root.as_os_str().as_encoded_bytes());
    hasher.update(b"\n");
    hasher.update(agent_home.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let name: String = std::iter::once(CONTAINER_PREFIX.to_owned())
        .chain(
            digest[..CONTAINER_HASH_BYTES]
                .iter()
                .map(|byte| format!("{byte:02x}")),
        )
        .collect();
    assert_eq!(
        name.len(),
        CONTAINER_PREFIX.len() + 2 * CONTAINER_HASH_BYTES
    );
    name
}

/// Where the boundary helper ships: beside the engine's own executable. Output: the path, or
/// the error reading the executable's path.
pub fn helper_beside_engine() -> std::io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name(HELPER_NAME))
}

/// The boundary helper at `helper`, when it is an existing absolute plain file. Output: the
/// path, `Unavailable` with [`APPCONTAINER_MISSING`] when it is missing, or `Invalid`.
pub fn existing_helper(helper: &Path) -> Result<PathBuf, SandboxError> {
    if !helper.is_file() {
        return Err(SandboxError::Unavailable(APPCONTAINER_MISSING.into()));
    }
    if !clean_absolute(helper) {
        return Err(invalid(
            "the boundary helper must be an absolute plain file",
        ));
    }
    Ok(helper.to_path_buf())
}

/// The Windows boundary rows on the stand-in hooks: profiles, installs, wrapped agents
/// inside their AppContainer and the owner's git.
#[cfg(all(test, windows))]
mod engine_rows;
/// The owner's own git for one project inside an AppContainer of its own, with no capability,
/// so a `.git` that points git elsewhere reaches nothing (V244-WIN-GIT-UNC-SIBLINGS).
#[cfg(windows)]
pub mod owner_git;
#[cfg(not(windows))]
#[path = "windows/owner_git_none.rs"]
pub mod owner_git;
#[cfg(windows)]
pub mod profile;
#[cfg(test)]
mod tests;
