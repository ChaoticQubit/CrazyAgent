use super::process::{file_sha256, is_sha256, private_folder, run, staging_folder, version_from};
use super::{
    Dependency, GRAPHIFY_PYTHON, InstallError, Installer, LOCK_FILE, Pin, ToolLock, Version,
    keep_only,
};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const CURL: &str = "/usr/bin/curl";
const TAR: &str = "/usr/bin/tar";
const ARCHIVE_BYTES_MAX: u64 = 128 * 1024 * 1024;
const VERSION_TIME: Duration = Duration::from_secs(10);
const DOWNLOAD_TIME: Duration = Duration::from_secs(300);
const UV_TOOL_TIME: Duration = Duration::from_secs(900);
const NAME_BYTES_MAX: usize = 64;

/// The folders searched after `setup/bin` on a Unix system: the owner's `~/.local/bin`,
/// the engine's own absolute `PATH` folders, then the platform's fixed folders.
/// Input: the fixed folders. Output: the folders in search order.
pub fn system_folders(fixed: &[&str]) -> Vec<PathBuf> {
    let mut system: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        system.push(PathBuf::from(home).join(".local/bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        system.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
    }
    system.extend(fixed.iter().map(PathBuf::from));
    system
}

/// Finds and installs uv and Graphify the same way on every Unix system. uv comes from
/// its release archive, kept only when its SHA-256 matches the pin; Graphify and other
/// Python tools come from their bundled hash locks. Installs go into `<engine root>/setup`:
/// programs in `setup/bin`, tool environments, cache and Python under `setup/uv`. Lookups
/// search `setup/bin` first, then the system folders. Nothing runs with sudo.
pub struct PinnedInstaller {
    setup: PathBuf,
    search: Vec<PathBuf>,
}

impl PinnedInstaller {
    /// Uses the engine root's `setup` folder and searches it, then the given absolute
    /// system folders. Inputs: the engine root and the folders searched after `setup/bin`.
    /// Output: the installer.
    pub fn with_paths(engine_root: &Path, system: Vec<PathBuf>) -> Self {
        let setup = engine_root.join("setup");
        let mut search = vec![setup.join("bin")];
        for dir in system {
            if dir.is_absolute() && !search.contains(&dir) {
                search.push(dir);
            }
        }
        assert!(!search.is_empty());
        Self { setup, search }
    }

    /// Returns the private install folder, `<engine root>/setup`.
    pub fn setup_folder(&self) -> &Path {
        &self.setup
    }

    /// Finds the first runnable copy of a program on the search folders and its version.
    /// Input: the dependency. Output: its path and version, or none when no copy reports one.
    pub fn locate(&self, dep: Dependency) -> Option<(PathBuf, Version)> {
        self.search.iter().find_map(|dir| {
            let candidate = dir.join(dep.program());
            if !runnable(&candidate) {
                return None;
            }
            self.version(dep, &candidate)
                .ok()
                .map(|version| (candidate, version))
        })
    }

    /// Returns the `PATH` value that puts `setup/bin` first, for programs the engine runs.
    pub fn search_path(&self) -> OsString {
        std::env::join_paths(&self.search).unwrap_or_default()
    }

    /// The only environment the programs setup starts get: `PATH` with `setup/bin` first,
    /// and the engine's `HOME`, `TMPDIR` and `LANG` when set. Output: the pairs.
    pub(super) fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut env = vec![("PATH", self.search_path())];
        for name in ["HOME", "TMPDIR", "LANG"] {
            if let Some(value) = std::env::var_os(name) {
                env.push((name, value));
            }
        }
        env
    }

    fn version(&self, dep: Dependency, program: &Path) -> Result<Version, InstallError> {
        let flag = match dep {
            Dependency::Tmux => "-V",
            Dependency::Uv | Dependency::Graphify => "--version",
        };
        let (ok, output) = run(
            program,
            &[OsString::from(flag)],
            &self.env(),
            &std::env::temp_dir(),
            VERSION_TIME,
        )?;
        if !ok {
            return Err(InstallError("setup_version_unreadable"));
        }
        version_from(&output)
    }

    fn release(
        &self,
        dep: Dependency,
        version: &Version,
        url: &str,
        sha256: &str,
        member: &str,
    ) -> Result<(), InstallError> {
        let member_path = Path::new(member);
        let valid = (url.starts_with("https://") || url.starts_with("file:///"))
            && url.len() <= 2048
            && !url.chars().any(|c| c.is_control() || c.is_whitespace())
            && is_sha256(sha256)
            && !member.starts_with('-')
            && member_path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && member_path.file_name() == Some(std::ffi::OsStr::new(dep.program()));
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let bin = self.setup.join("bin");
        private_folder(&bin)?;
        let staging = staging_folder(&self.setup, "download")?;
        let result = (|| {
            let archive = staging.join("archive.tar.gz");
            let (fetched, _) = run(
                Path::new(CURL),
                &[
                    "-q".into(),
                    "-fsSL".into(),
                    "--proto".into(),
                    "=https,file".into(),
                    "--max-filesize".into(),
                    ARCHIVE_BYTES_MAX.to_string().into(),
                    "--max-time".into(),
                    DOWNLOAD_TIME.as_secs().to_string().into(),
                    "-o".into(),
                    archive.clone().into_os_string(),
                    url.into(),
                ],
                &self.env(),
                &staging,
                DOWNLOAD_TIME + Duration::from_secs(5),
            )?;
            let size = std::fs::symlink_metadata(&archive)
                .map(|metadata| metadata.is_file().then_some(metadata.len()))
                .ok()
                .flatten();
            if !fetched || size.is_none_or(|size| size > ARCHIVE_BYTES_MAX) {
                return Err(InstallError("setup_download_failed"));
            }
            if file_sha256(&archive)? != sha256 {
                return Err(InstallError("setup_checksum_mismatch"));
            }
            let out = staging.join("out");
            private_folder(&out)?;
            let (unpacked, _) = run(
                Path::new(TAR),
                &[
                    "-xzf".into(),
                    archive.clone().into_os_string(),
                    "-C".into(),
                    out.clone().into_os_string(),
                    member.into(),
                ],
                &self.env(),
                &staging,
                DOWNLOAD_TIME,
            )?;
            let program = out.join(member_path);
            let regular = std::fs::symlink_metadata(&program).is_ok_and(|m| m.is_file());
            if !unpacked || !regular {
                return Err(InstallError("setup_install_failed"));
            }
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| InstallError("setup_store_unavailable"))?;
            if self.version(dep, &program)? != *version {
                return Err(InstallError("setup_version_mismatch"));
            }
            std::fs::rename(&program, bin.join(dep.program()))
                .map_err(|_| InstallError("setup_store_unavailable"))
        })();
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    fn uv_tool(&self, package: &str, version: &Version, python: &str) -> Result<(), InstallError> {
        if !plain_name(package) {
            return Err(InstallError("setup_pin_invalid"));
        }
        let lock = super::tool_lock(package, version.as_str())
            .ok_or(InstallError("setup_pin_unlocked"))?;
        self.install_locked(&lock, python)
    }

    /// Installs one Python tool exactly as its lock pins it: a fresh virtual environment
    /// under `setup/uv/tools/<package>`, into which uv installs only wheels whose SHA-256
    /// the lock lists, then links to its programs in `setup/bin`. A tool already linked
    /// from the same lock is kept. A failed install removes its environment and leaves
    /// the old links. Inputs: the lock and the Python version. Output: `setup_install_failed`
    /// when uv refuses a file or a program is missing, or another setup code.
    pub(super) fn install_locked(
        &self,
        lock: &ToolLock<'_>,
        python: &str,
    ) -> Result<(), InstallError> {
        let valid = plain_name(lock.package)
            && !lock.programs.is_empty()
            && lock.programs.iter().all(|program| plain_name(program))
            && !lock.requirements.is_empty()
            && !python.is_empty()
            && python.len() <= 8
            && python.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let bin = self.setup.join("bin");
        let tools = self.setup.join("uv/tools");
        let home = tools.join(lock.package);
        if self.linked_from(&home, lock) {
            return Ok(());
        }
        let (uv, _) = self
            .locate(Dependency::Uv)
            .ok_or(InstallError("setup_uv_missing"))?;
        let cache = self.setup.join("uv/cache");
        let pythons = self.setup.join("uv/python");
        let python_bin = self.setup.join("uv/python-bin");
        for folder in [&bin, &tools, &home, &cache, &pythons, &python_bin] {
            private_folder(folder)?;
        }
        let mut env = self.env();
        env.extend([
            ("UV_CACHE_DIR", cache.into_os_string()),
            ("UV_PYTHON_INSTALL_DIR", pythons.into_os_string()),
            ("UV_PYTHON_BIN_DIR", python_bin.into_os_string()),
            ("UV_NO_CONFIG", "1".into()),
            ("UV_NO_PROGRESS", "1".into()),
        ]);
        let staged = staging_folder(&home, "env")?;
        let result = (|| {
            let requirements = staged.join(LOCK_FILE);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&requirements)
                .and_then(|mut file| file.write_all(lock.requirements.as_bytes()))
                .map_err(|_| InstallError("setup_store_unavailable"))?;
            let venv = staged.join("venv");
            let commands =
                super::locked_tool_args(python, &venv, &venv.join("bin/python"), &requirements);
            for args in &commands {
                let (ok, _) = run(&uv, args, &env, &self.setup, UV_TOOL_TIME)?;
                if !ok {
                    return Err(InstallError("setup_install_failed"));
                }
            }
            let programs: Vec<PathBuf> = lock
                .programs
                .iter()
                .map(|program| venv.join("bin").join(program))
                .collect();
            let present = programs.iter().all(|program| {
                runnable(program) && std::fs::symlink_metadata(program).is_ok_and(|m| m.is_file())
            });
            if !present {
                return Err(InstallError("setup_install_failed"));
            }
            for (program, name) in programs.iter().zip(lock.programs) {
                link(program, &bin.join(name))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => keep_only(&home, &staged),
            Err(_) => {
                let _ = std::fs::remove_dir_all(&staged);
            }
        }
        result
    }

    fn linked_from(&self, home: &Path, lock: &ToolLock<'_>) -> bool {
        lock.programs.iter().all(|name| {
            let Ok(target) = std::fs::read_link(self.setup.join("bin").join(name)) else {
                return false;
            };
            let env = target.ancestors().nth(3);
            target.ends_with(Path::new("venv/bin").join(name))
                && env.and_then(Path::parent) == Some(home)
                && runnable(&target)
                && env.is_some_and(|env| {
                    std::fs::read(env.join(LOCK_FILE))
                        .is_ok_and(|text| text == lock.requirements.as_bytes())
                })
        })
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'.'
        })
}

fn link(target: &Path, at: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    let random = crate::engine::owner::random_id().map_err(|_| unavailable)?;
    let name = at.file_name().ok_or(unavailable)?.to_string_lossy();
    let staged = at.with_file_name(format!(".{name}-{}", &random[..24]));
    std::os::unix::fs::symlink(target, &staged).map_err(|_| unavailable)?;
    std::fs::rename(&staged, at).map_err(|_| {
        let _ = std::fs::remove_file(&staged);
        unavailable
    })
}

/// Tells whether a path is a regular file someone may run. Input: the path.
/// Output: true for a file with any execute bit, following links.
pub(super) fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
#[path = "pinned_tests.rs"]
mod tests;

impl Installer for PinnedInstaller {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
        Ok(self.locate(dep).map(|(_, version)| version))
    }

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        private_folder(&self.setup)?;
        self.uv_tool(package, &Version::parse(version)?, GRAPHIFY_PYTHON)
    }

    fn program(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return None;
        }
        self.search
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| runnable(candidate))
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        private_folder(&self.setup)?;
        let pinned = match pin {
            Pin::Release {
                version,
                url,
                sha256,
                member,
            } => {
                self.release(dep, version, url, sha256, member)?;
                version
            }
            Pin::UvTool {
                package,
                version,
                python,
            } => {
                self.uv_tool(package, version, python)?;
                version
            }
            Pin::Homebrew { .. } | Pin::Winget { .. } => {
                return Err(InstallError("setup_pin_invalid"));
            }
        };
        let (_, found) = self
            .locate(dep)
            .ok_or(InstallError("setup_install_failed"))?;
        if *pinned != found {
            return Err(InstallError("setup_version_mismatch"));
        }
        Ok(found)
    }
}
