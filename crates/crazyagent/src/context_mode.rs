use crate::file_lock::HeldLock;
use crate::plugins::ServerSpec;
use crate::setup::InstallError;
use crate::setup::process::{private_folder, run_command, staging_folder};
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PACKAGE: &str = "context-mode";
const PROGRAM: &str = "cli.bundle.mjs";
const MODULES: &str = "node_modules";
const VERSION_BYTES_MAX: usize = 32;
const MANIFEST_BYTES_MAX: u64 = 64 * 1024;
const LOCK_BYTES_MAX: u64 = 1024 * 1024;
const MARKER_BYTES_MAX: u64 = 32;
const INSTALL_TIME: Duration = Duration::from_secs(900);
const INSTALL_BACKOFF: Duration = Duration::from_secs(600);
const INSTALLS_MAX: usize = 16;

static INSTALLING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The folder holding the engine's one shared install of a context-mode version,
/// `<engine root>/setup/npm/context-mode@<version>`. Inputs: the engine root and the
/// version. Output: the path, which may not exist yet.
pub fn install_folder(root: &Path, version: &str) -> PathBuf {
    root.join("setup")
        .join("npm")
        .join(format!("{PACKAGE}@{version}"))
}

/// The spec an agent launch runs for one approved server. The default pack's pinned
/// context-mode launch, `<folder>/npx -y context-mode@<version>`, runs the engine's shared
/// install of that same version with the `node` beside npx once the install is complete, so
/// a fresh agent home never fetches the package into its own cold npm cache. Until then it
/// stays the approved npx launch and the engine installs the shared copy in the background.
/// Every other server is unchanged. Inputs: the engine root and the spec. Output: the spec.
pub fn for_launch(root: &Path, spec: ServerSpec) -> ServerSpec {
    let Some((bin, version)) = pinned(&spec) else {
        return spec;
    };
    if let Some(shared) = shared(&install_folder(root, version), &bin.join("node"), version) {
        return shared;
    }
    begin_install(root, bin, version);
    spec
}

/// The shared install folder a launch spec from [`for_launch`] runs from, which an agent's
/// sandbox must let it read whole, dependencies included. Input: the launch spec. Output:
/// the install folder, or none for any other spec.
pub fn shared_install(spec: &ServerSpec) -> Option<&Path> {
    let ServerSpec::Stdio { args, .. } = spec else {
        return None;
    };
    let [program] = args.as_slice() else {
        return None;
    };
    let program = Path::new(program);
    let package = program.parent()?;
    let modules = package.parent()?;
    let folder = modules.parent()?;
    let named = program.is_absolute()
        && program.file_name() == Some(PROGRAM.as_ref())
        && package.file_name() == Some(PACKAGE.as_ref())
        && modules.file_name() == Some(MODULES.as_ref())
        && folder.parent().and_then(Path::file_name) == Some("npm".as_ref())
        && folder
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(PACKAGE))
            .and_then(|name| name.strip_prefix('@'))
            .is_some_and(exact_version);
    named.then_some(folder)
}

/// Installs one context-mode version into the engine's shared folder, once. An exclusive
/// lock on `<folder>.lock` makes a parallel installer wait, then find the finished install.
/// Only a version the default pack pins with a tarball hash installs. Under the lock a failure
/// less than [`INSTALL_BACKOFF`] old (`<folder>.failed`) stops a retry, and whatever is left
/// at the install path that is not complete is removed. npm from npx's folder runs in an agent
/// sandbox (HTTPS only, a fresh home and cache) into a staging folder that becomes the install
/// folder only after the package and npm's recorded tarball hash check out.
/// Inputs: the engine root, npx's folder and the version. Output: the folder, or the code.
pub fn install(root: &Path, bin: &Path, version: &str) -> Result<PathBuf, InstallError> {
    if !exact_version(version) {
        return Err(InstallError("context_mode_version_invalid"));
    }
    if pin(version).is_none() {
        return Err(InstallError("context_mode_integrity_unpinned"));
    }
    if !runnable(&bin.join("npm")) || !runnable(&bin.join("node")) {
        return Err(InstallError("context_mode_npm_missing"));
    }
    let folder = install_folder(root, version);
    let parent = folder
        .parent()
        .ok_or(InstallError("setup_store_unavailable"))?;
    private_folder(parent)?;
    let prefix = format!("{PACKAGE}@{version}");
    let _lock = lock(&parent.join(format!("{prefix}.lock")))?;
    if complete(&folder, version) {
        return Ok(folder);
    }
    let failed = parent.join(format!("{prefix}.failed"));
    if backing_off(&failed) {
        return Err(InstallError("context_mode_install_backoff"));
    }
    let result = replace(root, &folder, bin, version);
    if result.is_ok() {
        let _ = std::fs::remove_file(&failed);
    } else {
        let _ = std::fs::write(&failed, unix_now().to_string());
    }
    result.map(|()| folder)
}

fn replace(root: &Path, folder: &Path, bin: &Path, version: &str) -> Result<(), InstallError> {
    let parent = folder
        .parent()
        .ok_or(InstallError("setup_store_unavailable"))?;
    let prefix = format!("{PACKAGE}@{version}");
    clear(folder)?;
    remove_staged(parent, &prefix);
    let staged = staging_folder(parent, &prefix)?;
    let work = staging_folder(parent, &format!("{prefix}-work"));
    let result = work.and_then(|work| {
        let filled = fill(root, &staged, &work, bin, version, prefix.as_ref())
            .and_then(|()| checked(&staged, version).map_err(InstallError))
            .and_then(|()| {
                std::fs::rename(&staged, folder)
                    .map_err(|_| InstallError("setup_store_unavailable"))
            });
        let _ = std::fs::remove_dir_all(&work);
        filled
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    result
}

fn pinned(spec: &ServerSpec) -> Option<(&Path, &str)> {
    let ServerSpec::Stdio { command, args } = spec else {
        return None;
    };
    let command = Path::new(command);
    let [flag, package] = args.as_slice() else {
        return None;
    };
    let version = package
        .strip_prefix(PACKAGE)
        .and_then(|rest| rest.strip_prefix('@'))
        .filter(|version| flag == "-y" && exact_version(version))?;
    let npx = command.is_absolute() && command.file_name() == Some("npx".as_ref());
    command.parent().filter(|_| npx).map(|bin| (bin, version))
}

fn exact_version(version: &str) -> bool {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    version.len() <= VERSION_BYTES_MAX
        && numbers.len() == 3
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|pre| {
            !pre.is_empty()
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        })
}

fn pin(version: &str) -> Option<String> {
    let wanted = format!("{PACKAGE}@{version}");
    crate::setup::pack::default_pack()
        .ok()?
        .entries
        .into_iter()
        .find(|entry| {
            entry.id == PACKAGE && entry.install.split_whitespace().any(|word| word == wanted)
        })?
        .integrity
}

fn shared(folder: &Path, node: &Path, version: &str) -> Option<ServerSpec> {
    if !folder.is_absolute() || !complete(folder, version) || !runnable(node) {
        return None;
    }
    let program = folder.join(MODULES).join(PACKAGE).join(PROGRAM);
    let spec = ServerSpec::Stdio {
        command: node.to_str()?.to_owned(),
        args: vec![program.to_str()?.to_owned()],
    };
    assert!(shared_install(&spec) == Some(folder));
    Some(spec)
}

fn complete(folder: &Path, version: &str) -> bool {
    checked(folder, version).is_ok()
}

fn checked(folder: &Path, version: &str) -> Result<(), &'static str> {
    let package = folder.join(MODULES).join(PACKAGE);
    let real = std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir());
    let program =
        std::fs::symlink_metadata(package.join(PROGRAM)).is_ok_and(|metadata| metadata.is_file());
    let reported = reported_version(&package.join("package.json"));
    if !real || !program || reported.as_deref() != Some(version) {
        return Err("context_mode_install_incomplete");
    }
    let pinned = pin(version).ok_or("context_mode_integrity_unpinned")?;
    if recorded_integrity(folder, version).as_deref() != Some(pinned.as_str()) {
        return Err("context_mode_integrity_mismatch");
    }
    Ok(())
}

fn recorded_integrity(folder: &Path, version: &str) -> Option<String> {
    let mut text = String::new();
    File::open(folder.join(MODULES).join(".package-lock.json"))
        .ok()?
        .take(LOCK_BYTES_MAX)
        .read_to_string(&mut text)
        .ok()?;
    let lock: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = &lock["packages"][format!("{MODULES}/{PACKAGE}")];
    if entry["version"] != version {
        return None;
    }
    entry["integrity"].as_str().map(str::to_owned)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn backing_off(marker: &Path) -> bool {
    let mut text = String::new();
    let read =
        File::open(marker).and_then(|file| file.take(MARKER_BYTES_MAX).read_to_string(&mut text));
    let Some(failed) = read.ok().and_then(|_| text.trim().parse::<u64>().ok()) else {
        return false;
    };
    let now = unix_now();
    let backoff = INSTALL_BACKOFF.as_secs();
    failed <= now.saturating_add(backoff) && now.saturating_sub(failed) < backoff
}

fn clear(folder: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    match std::fs::symlink_metadata(folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(unavailable),
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(folder).map_err(|_| unavailable)
        }
        Ok(_) => std::fs::remove_file(folder).map_err(|_| unavailable),
    }
}

fn reported_version(manifest: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(manifest)
        .ok()?
        .take(MANIFEST_BYTES_MAX)
        .read_to_string(&mut text)
        .ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    if manifest["name"] != PACKAGE {
        return None;
    }
    manifest["version"].as_str().map(str::to_owned)
}

fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn begin_install(root: &Path, bin: &Path, version: &str) {
    let folder = install_folder(root, version);
    {
        let mut installing = INSTALLING.lock().unwrap_or_else(PoisonError::into_inner);
        if installing.contains(&folder) || installing.len() >= INSTALLS_MAX {
            return;
        }
        installing.push(folder.clone());
        assert!(installing.len() <= INSTALLS_MAX);
    }
    let (root, bin, version) = (root.to_path_buf(), bin.to_path_buf(), version.to_owned());
    let held = folder.clone();
    let started = std::thread::Builder::new()
        .name("crazyagent-context-mode".into())
        .spawn(move || {
            let _ = install(&root, &bin, &version);
            finish(&held);
        });
    if started.is_err() {
        finish(&folder);
    }
}

fn finish(folder: &Path) {
    INSTALLING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|held| held != folder);
}

fn lock(path: &Path) -> Result<HeldLock, InstallError> {
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

fn remove_staged(parent: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let staged = format!(".{prefix}-");
    for entry in entries.flatten() {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&staged));
        if ours && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn refused<E>(_: E) -> InstallError {
    InstallError("context_mode_sandbox_unavailable")
}

/// Installs `source` into the staging folder with package scripts off, checks npm's record
/// of the tarball hash against the pin before any package script can run or rewrite it,
/// then runs the scripts with `npm rebuild`. Both npm runs share one time limit. Inputs:
/// the engine root, staging and work folders, npm's folder, the version and what npm
/// installs. Output: Ok once rebuilt; the caller re-checks before the rename.
fn fill(
    root: &Path,
    staged: &Path,
    work: &Path,
    bin: &Path,
    version: &str,
    source: &OsStr,
) -> Result<(), InstallError> {
    let deadline = Instant::now() + INSTALL_TIME;
    let npm = |task: &str, extra: &[&OsStr]| -> Result<(), InstallError> {
        let mut command = npm_command(root, staged, work, bin, version)?;
        command
            .arg(task)
            .arg("--prefix")
            .arg(staged)
            .args([
                "--no-audit",
                "--no-fund",
                "--no-update-notifier",
                "--loglevel=error",
            ])
            .args(extra);
        match run_command(command, deadline.saturating_duration_since(Instant::now()))? {
            (true, _) => Ok(()),
            (false, _) => Err(InstallError("context_mode_install_failed")),
        }
    };
    npm(
        "install",
        &[
            OsStr::new("--ignore-scripts"),
            OsStr::new("--no-save"),
            source,
        ],
    )?;
    checked(staged, version).map_err(InstallError)?;
    npm("rebuild", &[])
}

fn npm_command(
    root: &Path,
    staged: &Path,
    work: &Path,
    bin: &Path,
    version: &str,
) -> Result<std::process::Command, InstallError> {
    use crate::sandbox::toolchains::{ToolHomes, Toolchain, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork, SandboxRemote};
    let parent = staged
        .parent()
        .ok_or(InstallError("setup_store_unavailable"))?;
    let home = work.join("home");
    let tmp = work.join("tmp");
    private_folder(&home)?;
    private_folder(&tmp)?;
    let canonical = |path: &Path| path.canonicalize().map_err(refused);
    let homes = ToolHomes::owner().map_err(refused)?;
    let sandbox = ProjectSandbox::new(
        &canonical(staged)?,
        &canonical(&home)?,
        &canonical(&tmp)?,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    )
    .and_then(|sandbox| sandbox.guard_engine(root))
    .and_then(|sandbox| sandbox.allow_tools(&Toolchains::grant(&homes, &[Toolchain::Node])))
    .map_err(refused)?;
    let sandbox = sandbox.allow_read(&canonical(bin)?).map_err(refused)?;
    let profile = parent.join(format!("{PACKAGE}@{version}.sb"));
    sandbox.install(&profile).map_err(refused)?;
    let mut command = sandbox.command(&profile, &bin.join("npm"));
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

#[cfg(test)]
mod tests;
