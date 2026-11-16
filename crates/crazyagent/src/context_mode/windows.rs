use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use crate::setup::InstallError;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The name of the `node` program beside npx.
pub(super) const NODE: &str = "node.exe";

/// The lock an install holds on `<folder>.lock` while it runs.
pub(super) type Lock = File;

/// Stub: no file counts as runnable yet. Input: the path. Output: false.
pub(super) fn runnable(_path: &Path) -> bool {
    false
}

/// Stub: npm never counts as ready yet. Input: npx's folder. Output: false.
pub(super) fn npm_ready(_bin: &Path) -> bool {
    false
}

/// Stub: the install's npm reaches nothing yet. Output: no endpoint.
pub(super) fn npm_network() -> SandboxNetwork {
    SandboxNetwork::Only(Vec::new())
}

/// Stub: no program file opens yet. Input: the path. Output: none.
pub(super) fn open_plain(_path: &Path) -> Option<File> {
    None
}

/// Stub: no install lock is taken yet. Input: the lock file. Output: the refusal.
pub(super) fn lock(_path: &Path) -> Result<Lock, InstallError> {
    Err(InstallError("setup_store_unavailable"))
}

/// Stub: no private folder is made yet. Input: the folder. Output: the refusal.
pub(super) fn private_folder(_path: &Path) -> Result<(), InstallError> {
    Err(InstallError("setup_store_unavailable"))
}

/// Stub: no staging folder is made yet. Inputs: the parent and the prefix. Output: the
/// refusal.
pub(super) fn staging_folder(_parent: &Path, _prefix: &str) -> Result<PathBuf, InstallError> {
    Err(InstallError("setup_store_unavailable"))
}

/// Stub: no bounded step runs yet. Inputs: the command and the limit. Output: the refusal.
pub(super) fn run_command(
    _command: Command,
    _limit: Duration,
) -> Result<(bool, Vec<u8>), InstallError> {
    Err(InstallError("setup_install_failed"))
}

/// Stub: no quiet step runs yet. Inputs: the command and the limit. Output: the refusal.
pub(super) fn run_quiet(_command: Command, _limit: Duration) -> Result<bool, InstallError> {
    Err(InstallError("setup_install_failed"))
}

/// Stub: npm's command before its boundary still names `npm.cmd`. Inputs: npx's folder and
/// npm's working folder. Output: the command.
pub(super) fn npm_invocation(bin: &Path, _cwd: &Path) -> Command {
    Command::new(bin.join("npm.cmd"))
}

/// Stub: npm never starts in a boundary yet. Inputs: the boundary, the engine root, the
/// profile path, npx's folder and npm's working folder. Output: the refusal.
pub(super) fn npm_wrap(
    _sandbox: ProjectSandbox,
    _root: &Path,
    _profile: &Path,
    _bin: &Path,
    _cwd: &Path,
) -> Result<Command, InstallError> {
    Err(InstallError("context_mode_sandbox_unavailable"))
}
