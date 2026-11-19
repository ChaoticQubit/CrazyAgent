use super::refused;
use crate::file_lock::HeldLock;
use crate::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use crate::sandbox_settings::SandboxSettings;
use crate::setup::InstallError;
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub(super) use crate::setup::process::{private_folder, run_command, staging_folder};

/// The name of the `node` program beside npx.
pub(super) const NODE: &str = "node";

/// The lock an install holds on `<folder>.lock` while it runs.
pub(super) type Lock = HeldLock;

/// Runs one install step whose output nobody reads, bounded like [`run_command`]. Inputs:
/// the command and the bound. Output: whether it exited with success, or `setup_timeout` or
/// `setup_install_failed`.
pub(super) fn run_quiet(command: Command, limit: Duration) -> Result<bool, InstallError> {
    run_command(command, limit).map(|(passed, _)| passed)
}

/// Whether a path is a file with an execute bit set. Input: the path. Output: the answer.
pub(super) fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Whether npm beside npx can run: `<folder>/npm` with an execute bit. Input: npx's folder.
/// Output: the answer.
pub(super) fn npm_ready(bin: &Path) -> bool {
    runnable(&bin.join("npm"))
}

/// The network the install's npm may reach: HTTPS on any host, nothing else.
pub(super) fn npm_network() -> SandboxNetwork {
    SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)])
}

/// Opens a file to read without following a link at its path or waiting on a FIFO there.
/// Input: the path. Output: the file, or none when it does not open or is not a regular file.
pub(super) fn open_plain(path: &Path) -> Option<File> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = File::from(rustix::fs::open(path, flags, Mode::empty()).ok()?);
    file.metadata()
        .is_ok_and(|metadata| metadata.is_file())
        .then_some(file)
}

/// Takes the exclusive lock on an install's lock file, created 0600 with no link followed,
/// waiting while another installer holds it. Input: the lock file. Output: the held lock, or
/// `setup_store_unavailable`.
pub(super) fn lock(path: &Path) -> Result<Lock, InstallError> {
    HeldLock::take(open_lock(path)?, FlockOperation::LockExclusive)
        .map_err(|_| InstallError("setup_store_unavailable"))
}

/// Opens a lock file, created 0600 when missing, with no link followed, and keeps it only
/// when it is a regular file. Input: the lock file. Output: the file, or
/// `setup_store_unavailable`.
fn open_lock(path: &Path) -> Result<File, InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| unavailable)?,
    );
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(unavailable);
    }
    Ok(file)
}

/// Takes a shared lock on a lock file, waiting only while an exclusive holder has it. Input:
/// the lock file. Output: the held lock, or `setup_store_unavailable`.
pub(super) fn lock_shared(path: &Path) -> Result<Lock, InstallError> {
    HeldLock::take(open_lock(path)?, FlockOperation::LockShared)
        .map_err(|_| InstallError("setup_store_unavailable"))
}

/// Takes the exclusive lock on a lock file only when no other open file holds it, never
/// waiting. Input: the lock file. Output: the held lock, none while another holds it, or
/// `setup_store_unavailable`.
pub(super) fn try_lock(path: &Path) -> Result<Option<Lock>, InstallError> {
    match HeldLock::take(open_lock(path)?, FlockOperation::NonBlockingLockExclusive) {
        Ok(held) => Ok(Some(held)),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(_) => Err(InstallError("setup_store_unavailable")),
    }
}

/// The owner's sandbox settings a context-mode step's boundary takes on Linux (DEC-185): the
/// stored ones, call supervisor and strict choice included, so a store that cannot be read
/// refuses the step. Input: the engine root. Output: the settings, or
/// `context_mode_sandbox_unavailable`.
#[cfg(target_os = "linux")]
pub(super) fn boundary_settings(root: &Path) -> Result<SandboxSettings, InstallError> {
    crate::sandbox_settings::stored(root).map_err(refused)
}

/// The sandbox settings a context-mode step's boundary takes off Linux: the defaults, strict
/// off, which macOS ignores. Input: the engine root, unread. Output: the defaults.
#[cfg(not(target_os = "linux"))]
pub(super) fn boundary_settings(_root: &Path) -> Result<SandboxSettings, InstallError> {
    Ok(SandboxSettings::default())
}

/// Builds the install's npm command inside its installed boundary: Seatbelt on macOS, the
/// owner's socket and call supervisor choices on Linux. npx's folder leads PATH, because npm
/// is a script that `env` finds `node` for. Inputs: the boundary, the engine root, the
/// profile path, npx's folder and npm's working folder. Output: the command, or the refusal.
pub(super) fn npm_wrap(
    sandbox: ProjectSandbox,
    root: &Path,
    profile: &Path,
    bin: &Path,
    cwd: &Path,
) -> Result<Command, InstallError> {
    #[cfg(not(target_os = "linux"))]
    let mut command = {
        let _ = (root, cwd);
        sandbox.install(profile).map_err(refused)?;
        sandbox.command(profile, &bin.join("npm"))
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let settings = boundary_settings(root)?;
        let sandbox = sandbox.limit_sockets(settings.linux_sockets);
        let calls = settings.linux_supervisor;
        let strict = settings.linux_strict;
        let sandbox = crate::sandbox::InstalledSandbox::install(sandbox, profile, calls, strict)
            .map_err(refused)?;
        let mut npm = Command::new(bin.join("npm"));
        npm.current_dir(cwd);
        sandbox.wrap(&npm).map_err(refused)?
    };
    let path = command
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .map(|value| value.to_owned())
        .unwrap_or_default();
    let mut search = bin.as_os_str().to_owned();
    search.push(":");
    search.push(path);
    command
        .env("PATH", search)
        .env("npm_config_update_notifier", "false");
    Ok(command)
}

/// Reports nothing to release: Seatbelt and Landlock leave no access entry or container behind
/// a finished job. Inputs: the job's profile, folder and home, unused. Output: true.
pub(super) fn release(_profile: &Path, _folder: &Path, _home: &Path) -> bool {
    true
}
