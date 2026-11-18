use super::refused;
use crate::sandbox::{InstalledSandbox, LinuxSupervisor, ProjectSandbox, SandboxNetwork};
use crate::setup::InstallError;
use crate::setup::run::run_bounded;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// The name of the `node` program beside npx.
pub(super) const NODE: &str = "node.exe";

/// The exclusive lock an install holds on `<folder>.lock`, released when dropped rather than
/// whenever Windows gets round to it after the handle closes.
pub(super) struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a path is a file. Windows has no execute bits, so any file beside npx counts.
/// Input: the path. Output: the answer.
pub(super) fn runnable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// npm's own program in a Windows Node folder, `node_modules\npm\bin\npm-cli.js`, which
/// `node.exe` runs so cmd.exe never parses npm's arguments. Input: npx's folder. Output: its
/// path.
fn npm_cli(bin: &Path) -> PathBuf {
    bin.join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js")
}

/// Whether npm beside npx can run: `npm-cli.js` in its Node folder, never `npm.cmd`. Input:
/// npx's folder. Output: the answer.
pub(super) fn npm_ready(bin: &Path) -> bool {
    runnable(&npm_cli(bin))
}

/// The first drive root among `paths` whose list keeps every app container from reading its
/// attributes, so a boxed Node.js script there fails to resolve its own path
/// ([`crazyagent_win_sandbox::unreadable_drive_root`]). Input: the paths. Output: that root,
/// or none.
pub(super) fn unreadable_drive_root(paths: &[&Path]) -> Option<PathBuf> {
    crazyagent_win_sandbox::unreadable_drive_root(paths.iter().copied())
}

/// The network the install's npm may reach on Windows: open, for this step only, because the
/// pinned program needs npm's dependencies and the AppContainer widens any host limit to the
/// internet client anyway (DESIGN-0256 O2a). Scripts stay off and both pins are checked first.
pub(super) fn npm_network() -> SandboxNetwork {
    SandboxNetwork::Open
}

/// Opens a path as itself, never through a link or junction there, and keeps it only when it
/// is a regular file. Inputs: the open options and the path. Output: the file, or none.
fn open_no_link(options: &mut OpenOptions, path: &Path) -> Option<File> {
    let file = options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    file.metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .then_some(file)
}

/// Opens a file to read without following a link at its path. Input: the path. Output: the
/// file, or none when it does not open or is not a regular file.
pub(super) fn open_plain(path: &Path) -> Option<File> {
    open_no_link(OpenOptions::new().read(true), path)
}

/// Opens a lock file, created when missing, with no link followed. Input: the lock file.
/// Output: the file, or `setup_store_unavailable`.
fn open_lock(path: &Path) -> Result<File, InstallError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    open_no_link(&mut options, path).ok_or(InstallError("setup_store_unavailable"))
}

/// Takes the exclusive lock on an install's lock file, created with no link followed,
/// waiting while another installer holds it. Input: the lock file. Output: the held lock, or
/// `setup_store_unavailable`.
pub(super) fn lock(path: &Path) -> Result<Lock, InstallError> {
    let file = open_lock(path)?;
    file.lock()
        .map_err(|_| InstallError("setup_store_unavailable"))?;
    Ok(Lock(file))
}

/// Takes a shared lock on a lock file, waiting only while an exclusive holder has it. Input:
/// the lock file. Output: the held lock, or `setup_store_unavailable`.
pub(super) fn lock_shared(path: &Path) -> Result<Lock, InstallError> {
    let file = open_lock(path)?;
    file.lock_shared()
        .map_err(|_| InstallError("setup_store_unavailable"))?;
    Ok(Lock(file))
}

/// Takes the exclusive lock on a lock file only when no other handle holds it, never
/// waiting. Input: the lock file. Output: the held lock, none while another handle holds it,
/// or `setup_store_unavailable`.
pub(super) fn try_lock(path: &Path) -> Result<Option<Lock>, InstallError> {
    let file = open_lock(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(_)) => Err(InstallError("setup_store_unavailable")),
    }
}

/// Creates a folder and its parents, or checks an existing one, refusing a link or a file
/// there; the owner's ACL is inherited from the engine root. Input: the folder. Output:
/// `setup_store_unavailable` when it cannot be made or is not a real folder.
pub(super) fn private_folder(path: &Path) -> Result<(), InstallError> {
    crate::paths::private_folder(path).map_err(|_| InstallError("setup_store_unavailable"))
}

/// Makes a fresh staging folder inside a parent, named with a random suffix, never reusing
/// one that exists. Inputs: the parent folder and a name prefix. Output: the new folder.
pub(super) fn staging_folder(parent: &Path, prefix: &str) -> Result<PathBuf, InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    private_folder(parent)?;
    let random = crate::engine::owner::random_id().map_err(|_| unavailable)?;
    let path = parent.join(format!(".{prefix}-{}", &random[..24]));
    std::fs::create_dir(&path).map_err(|_| unavailable)?;
    Ok(path)
}

/// Reads one bounded step's outcome the way the unix runner reports it: a failed exit or a
/// start failure is a step that did not pass, a timeout stays an error. Input: the outcome.
/// Output: whether it passed and its output, or the error.
fn passed(outcome: Result<Vec<u8>, InstallError>) -> Result<(bool, Vec<u8>), InstallError> {
    match outcome {
        Ok(output) => Ok((true, output)),
        Err(InstallError("setup_install_failed")) => Ok((false, Vec::new())),
        Err(error) => Err(error),
    }
}

/// Runs one prepared command with no input and a time bound, keeping up to 4 KiB of its
/// output; the helper's job object ends the whole tree at the bound. Inputs: the command and
/// the bound. Output: whether it passed and its output, or `setup_timeout`.
pub(super) fn run_command(
    mut command: Command,
    limit: Duration,
) -> Result<(bool, Vec<u8>), InstallError> {
    passed(run_bounded(&mut command, limit, true))
}

/// Runs one install step with its output thrown away, so it never fills a pipe nobody reads
/// before it ends. Inputs: the command and the bound. Output: whether it passed, or
/// `setup_timeout`.
pub(super) fn run_quiet(mut command: Command, limit: Duration) -> Result<bool, InstallError> {
    passed(run_bounded(&mut command, limit, false)).map(|(ok, _)| ok)
}

/// The sandbox settings a context-mode step's boundary takes on Windows: the defaults,
/// strict off and the call supervisor's default, whatever the owner stored, since only Linux
/// reads them (DEC-185). Input: the engine root, unread. Output: the defaults.
pub(super) fn boundary_settings(
    _root: &Path,
) -> Result<crate::sandbox_settings::SandboxSettings, InstallError> {
    Ok(crate::sandbox_settings::SandboxSettings {
        linux_supervisor: LinuxSupervisor::On,
        linux_strict: crate::sandbox::LinuxStrict::Off,
        ..Default::default()
    })
}

/// npm's command before its boundary: `node.exe` with npm's `npm-cli.js`, in npm's working
/// folder, with npm's update check off. Inputs: npx's folder and npm's working folder.
/// Output: the command.
pub(super) fn npm_invocation(bin: &Path, cwd: &Path) -> Command {
    let mut npm = Command::new(bin.join(NODE));
    npm.arg(npm_cli(bin))
        .current_dir(cwd)
        .env("npm_config_update_notifier", "false");
    npm
}

/// Builds the install's npm command inside its installed boundary, the AppContainer and job
/// the helper starts. No PATH is added: `node.exe` and `npm-cli.js` are absolute and scripts
/// are off. Inputs: the boundary, the engine root, the profile path, npx's folder and npm's
/// working folder. Output: the command, or the refusal, Node.js's own code when npx's folder
/// is one no app container may read and this user cannot change.
pub(super) fn npm_wrap(
    sandbox: ProjectSandbox,
    root: &Path,
    profile: &Path,
    bin: &Path,
    cwd: &Path,
) -> Result<Command, InstallError> {
    let settings = boundary_settings(root)?;
    let (calls, strict) = (settings.linux_supervisor, settings.linux_strict);
    let real = crate::sandbox::canonical(bin).map_err(refused)?;
    let sandbox = InstalledSandbox::install(sandbox, profile, calls, strict)
        .map_err(|error| super::boundary_refused(&real, error))?;
    sandbox.wrap(&npm_invocation(bin, cwd)).map_err(refused)
}

/// Takes back what the boundary granted a finished job's container and deletes the container
/// ([`crazyagent_win_sandbox::release`]), then deletes the profile. The container is named
/// from the canonical job folder and home its launch used, read before the job ran, since the
/// job may rename or delete its own folders ([`crate::sandbox::windows::container_for`]). A
/// profile never written means nothing was granted. Inputs: the job's profile, folder and
/// home. Output: whether nothing is left; a profile that cannot be read or a refused release
/// is logged with its cause and kept.
pub(super) fn release(profile: &Path, folder: &Path, home: &Path) -> bool {
    assert!(folder.is_absolute(), "{}", folder.display());
    assert!(home.is_absolute(), "{}", home.display());
    match std::fs::symlink_metadata(profile) {
        Ok(_) => {}
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return true,
        Err(problem) => {
            eprintln!("context_mode_container_not_released cause={problem}");
            return false;
        }
    }
    let container = crate::sandbox::windows::container_for(folder, home);
    match crazyagent_win_sandbox::release(profile, &container) {
        Ok(()) => {
            let _ = std::fs::remove_file(profile);
            true
        }
        Err(cause) => {
            eprintln!("context_mode_container_not_released cause={cause}");
            false
        }
    }
}
