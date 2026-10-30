use crate::plugins::ServerSpec;
use crate::setup::InstallError;
use crate::setup::process::{private_folder, run_command, staging_folder};
use rustix::fs::{FlockOperation, Mode, OFlags};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

const PACKAGE: &str = "context-mode";
const PROGRAM: &str = "cli.bundle.mjs";
const MODULES: &str = "node_modules";
const VERSION_BYTES_MAX: usize = 32;
const MANIFEST_BYTES_MAX: u64 = 64 * 1024;
const INSTALL_TIME: Duration = Duration::from_secs(900);
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
            .is_some_and(plain_version);
    named.then_some(folder)
}

/// Installs one context-mode version into the engine's shared folder, once. An exclusive
/// lock on `<folder>.lock` makes a parallel installer wait, then find the finished install.
/// npm from npx's folder runs inside an agent sandbox with its own private home and cache,
/// as npx did in each agent before, into a staging folder that becomes the install folder
/// only after the package checks out. Inputs: the engine root, npx's folder and the version.
/// Output: the install folder, or the failure code.
pub fn install(root: &Path, bin: &Path, version: &str) -> Result<PathBuf, InstallError> {
    if !plain_version(version) {
        return Err(InstallError("context_mode_version_invalid"));
    }
    if !runnable(&bin.join("npm")) || !runnable(&bin.join("node")) {
        return Err(InstallError("context_mode_npm_missing"));
    }
    let folder = install_folder(root, version);
    let parent = folder
        .parent()
        .ok_or(InstallError("setup_store_unavailable"))?;
    private_folder(parent)?;
    let _lock = lock(&parent.join(format!("{PACKAGE}@{version}.lock")))?;
    if complete(&folder, version) {
        return Ok(folder);
    }
    let prefix = format!("{PACKAGE}@{version}");
    remove_staged(parent, &prefix);
    let staged = staging_folder(parent, &prefix)?;
    let result = fill(root, &staged, bin, version).and_then(|()| {
        if !complete(&staged, version) {
            return Err(InstallError("context_mode_install_incomplete"));
        }
        std::fs::rename(&staged, &folder).map_err(|_| InstallError("setup_store_unavailable"))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    result.map(|()| folder)
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
        .filter(|version| flag == "-y" && plain_version(version))?;
    let npx = command.is_absolute() && command.file_name() == Some("npx".as_ref());
    command.parent().filter(|_| npx).map(|bin| (bin, version))
}

fn plain_version(version: &str) -> bool {
    version.len() <= VERSION_BYTES_MAX
        && version
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
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
    let package = folder.join(MODULES).join(PACKAGE);
    let real = std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir());
    let program =
        std::fs::symlink_metadata(package.join(PROGRAM)).is_ok_and(|metadata| metadata.is_file());
    real && program && reported_version(&package.join("package.json")).as_deref() == Some(version)
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

fn lock(path: &Path) -> Result<File, InstallError> {
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
    rustix::fs::flock(&file, FlockOperation::LockExclusive).map_err(|_| unavailable)?;
    Ok(file)
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

fn fill(root: &Path, staged: &Path, bin: &Path, version: &str) -> Result<(), InstallError> {
    use crate::sandbox::toolchains::{ToolHomes, Toolchain, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    let parent = staged
        .parent()
        .ok_or(InstallError("setup_store_unavailable"))?;
    let home = parent.join("home");
    let tmp = parent.join("tmp");
    private_folder(&home)?;
    private_folder(&tmp)?;
    let canonical = |path: &Path| path.canonicalize().map_err(refused);
    let homes = ToolHomes::owner().map_err(refused)?;
    let sandbox = ProjectSandbox::new(
        &canonical(staged)?,
        &canonical(&home)?,
        &canonical(&tmp)?,
        SandboxNetwork::Open,
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
        .arg("install")
        .arg("--prefix")
        .arg(staged)
        .args([
            "--no-save",
            "--no-audit",
            "--no-fund",
            "--no-update-notifier",
            "--loglevel=error",
        ])
        .arg(format!("{PACKAGE}@{version}"));
    let (ok, _) = run_command(command, INSTALL_TIME)?;
    if !ok {
        return Err(InstallError("context_mode_install_failed"));
    }
    Ok(())
}
