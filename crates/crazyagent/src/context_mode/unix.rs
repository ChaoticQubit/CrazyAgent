use super::refused;
use crate::file_lock::HeldLock;
use crate::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
use crate::setup::InstallError;
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

pub(super) use crate::setup::process::{private_folder, run_command, staging_folder};

/// The name of the `node` program beside npx.
pub(super) const NODE: &str = "node";

/// The lock an install holds on `<folder>.lock` while it runs.
pub(super) type Lock = HeldLock;

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
    HeldLock::take(file, FlockOperation::LockExclusive).map_err(|_| unavailable)
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
        let settings = crate::sandbox_settings::stored(root).map_err(refused)?;
        let sandbox = sandbox.limit_sockets(settings.linux_sockets);
        let calls = settings.linux_supervisor;
        let sandbox =
            crate::sandbox::InstalledSandbox::install(sandbox, profile, calls).map_err(refused)?;
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
