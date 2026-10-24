use super::process::{file_sha256, is_sha256, private_folder, run, staging_folder, version_from};
use super::{Dependency, InstallError, Installer, Pin, Version};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const UV_VERSION: &str = "0.13.5";
const UV_ARM: (&str, &str) = (
    "uv-aarch64-apple-darwin",
    "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73",
);
const UV_INTEL: (&str, &str) = (
    "uv-x86_64-apple-darwin",
    "b0f52b0e3945b4107d89024ea38e625683011fbfc05a0d98f9fa2d6dcee28c66",
);
pub const GRAPHIFY_PACKAGE: &str = "graphifyy";
pub const GRAPHIFY_VERSION: &str = "0.9.9";
const GRAPHIFY_PYTHON: &str = "3.12";
const BREW: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];
const SYSTEM_PATH: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const CURL: &str = "/usr/bin/curl";
const TAR: &str = "/usr/bin/tar";
const ARCHIVE_BYTES_MAX: u64 = 128 * 1024 * 1024;
const VERSION_TIME: Duration = Duration::from_secs(10);
const DOWNLOAD_TIME: Duration = Duration::from_secs(300);
const UV_TOOL_TIME: Duration = Duration::from_secs(900);
const BREW_TIME: Duration = Duration::from_secs(1800);
const NAME_BYTES_MAX: usize = 64;

/// The pinned install of each dependency on this Mac: uv 0.13.5 from its GitHub release
/// archive with the published SHA-256, Graphify 0.9.9 through `uv tool install` on
/// Python 3.12, and tmux from Homebrew. Input: the dependency. Output: its pin.
pub fn pin(dep: Dependency) -> Pin {
    match dep {
        Dependency::Uv => {
            let (name, sha256) = if cfg!(target_arch = "aarch64") {
                UV_ARM
            } else {
                UV_INTEL
            };
            Pin::Release {
                version: Version(UV_VERSION.to_owned()),
                url: format!(
                    "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/{name}.tar.gz"
                ),
                sha256: sha256.to_owned(),
                member: format!("{name}/uv"),
            }
        }
        Dependency::Graphify => Pin::UvTool {
            package: GRAPHIFY_PACKAGE.to_owned(),
            version: Version(GRAPHIFY_VERSION.to_owned()),
            python: GRAPHIFY_PYTHON.to_owned(),
        },
        Dependency::Tmux => Pin::Homebrew {
            formula: "tmux".to_owned(),
        },
    }
}

/// Finds and installs dependencies on macOS. Installs go into `<engine root>/setup`:
/// programs in `setup/bin`, uv's tools, cache and Python under `setup/uv`. Lookups search
/// `setup/bin` first, then the system folders. Nothing runs with sudo and no global
/// configuration is read or changed by the installs it starts.
pub struct MacInstaller {
    setup: PathBuf,
    search: Vec<PathBuf>,
    brew: Vec<PathBuf>,
}

impl MacInstaller {
    /// Uses the engine root's `setup` folder and searches it, the owner's `~/.local/bin`,
    /// the engine's own absolute `PATH` folders and the usual Homebrew and system folders.
    /// Input: the engine root. Output: the installer.
    pub fn new(engine_root: &Path) -> Self {
        let mut system: Vec<PathBuf> = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            system.push(PathBuf::from(home).join(".local/bin"));
        }
        if let Some(path) = std::env::var_os("PATH") {
            system.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
        }
        system.extend(SYSTEM_PATH.iter().map(PathBuf::from));
        Self::with_paths(
            engine_root,
            system,
            BREW.iter().map(PathBuf::from).collect(),
        )
    }

    /// Same as `new` with the system folders and Homebrew programs given exactly.
    /// Inputs: the engine root, the folders searched after `setup/bin` and the absolute
    /// `brew` paths to try. Output: the installer.
    pub fn with_paths(engine_root: &Path, system: Vec<PathBuf>, brew: Vec<PathBuf>) -> Self {
        let setup = engine_root.join("setup");
        let mut search = vec![setup.join("bin")];
        for dir in system {
            if dir.is_absolute() && !search.contains(&dir) {
                search.push(dir);
            }
        }
        assert!(!search.is_empty());
        Self {
            setup,
            search,
            brew: brew.into_iter().filter(|path| path.is_absolute()).collect(),
        }
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

    fn env(&self) -> Vec<(&'static str, OsString)> {
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
        let valid = !package.is_empty()
            && package.len() <= NAME_BYTES_MAX
            && package.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'.'
            })
            && !python.is_empty()
            && python.len() <= 8
            && python.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let (uv, _) = self
            .locate(Dependency::Uv)
            .ok_or(InstallError("setup_uv_missing"))?;
        let bin = self.setup.join("bin");
        let tools = self.setup.join("uv/tools");
        let cache = self.setup.join("uv/cache");
        let pythons = self.setup.join("uv/python");
        let python_bin = self.setup.join("uv/python-bin");
        for folder in [&bin, &tools, &cache, &pythons, &python_bin] {
            private_folder(folder)?;
        }
        let mut env = self.env();
        env.extend([
            ("UV_TOOL_DIR", tools.into_os_string()),
            ("UV_TOOL_BIN_DIR", bin.into_os_string()),
            ("UV_CACHE_DIR", cache.into_os_string()),
            ("UV_PYTHON_INSTALL_DIR", pythons.into_os_string()),
            ("UV_PYTHON_BIN_DIR", python_bin.into_os_string()),
            ("UV_NO_CONFIG", "1".into()),
            ("UV_NO_PROGRESS", "1".into()),
        ]);
        let (ok, _) = run(
            &uv,
            &[
                "tool".into(),
                "install".into(),
                "--python".into(),
                python.into(),
                format!("{package}=={}", version.as_str()).into(),
            ],
            &env,
            &self.setup,
            UV_TOOL_TIME,
        )?;
        if !ok {
            return Err(InstallError("setup_install_failed"));
        }
        Ok(())
    }

    fn homebrew(&self, formula: &str) -> Result<(), InstallError> {
        let valid = !formula.is_empty()
            && formula.len() <= NAME_BYTES_MAX
            && formula.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'-' | b'_' | b'.' | b'@' | b'+')
            });
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let brew = self
            .brew
            .iter()
            .find(|path| runnable(path))
            .ok_or(InstallError("setup_homebrew_missing"))?;
        let mut env = self.env();
        env.extend([
            ("HOMEBREW_NO_AUTO_UPDATE", OsString::from("1")),
            ("HOMEBREW_NO_ANALYTICS", "1".into()),
            ("HOMEBREW_NO_INSTALL_CLEANUP", "1".into()),
            ("HOMEBREW_NO_ENV_HINTS", "1".into()),
        ]);
        let (ok, _) = run(
            brew,
            &["install".into(), formula.into()],
            &env,
            &std::env::temp_dir(),
            BREW_TIME,
        )?;
        if !ok {
            return Err(InstallError("setup_install_failed"));
        }
        Ok(())
    }
}

fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

impl Installer for MacInstaller {
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
                Some(version)
            }
            Pin::UvTool {
                package,
                version,
                python,
            } => {
                self.uv_tool(package, version, python)?;
                Some(version)
            }
            Pin::Homebrew { formula } => {
                self.homebrew(formula)?;
                None
            }
        };
        let (_, found) = self
            .locate(dep)
            .ok_or(InstallError("setup_install_failed"))?;
        if let Some(pinned) = pinned
            && *pinned != found
        {
            return Err(InstallError("setup_version_mismatch"));
        }
        Ok(found)
    }
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
