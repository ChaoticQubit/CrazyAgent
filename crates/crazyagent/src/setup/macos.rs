use super::pinned::{PinnedInstaller, runnable, system_folders};
use super::process::{private_folder, run};
use super::{Dependency, InstallError, Installer, Pin, Version};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use super::{GRAPHIFY_PACKAGE, GRAPHIFY_VERSION, UV_VERSION};

const BREW: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];
const SYSTEM_PATH: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const BREW_TIME: Duration = Duration::from_secs(1800);
const NAME_BYTES_MAX: usize = 64;

/// The pinned install of each dependency on this Mac: uv 0.13.5 from its GitHub release
/// archive with the published SHA-256, Graphify 0.9.9 through `uv tool install` on
/// Python 3.12, and tmux from Homebrew. Input: the dependency. Output: its pin.
pub fn pin(dep: Dependency) -> Pin {
    super::platform_pin(dep).expect("every macOS target pins every dependency")
}

/// Finds and installs dependencies on macOS. uv and Graphify go through the shared
/// pinned installer into `<engine root>/setup`; tmux comes from Homebrew. Lookups search
/// `setup/bin` first, then the system folders. Nothing runs with sudo and no global
/// configuration is read or changed by the installs it starts.
pub struct MacInstaller {
    pinned: PinnedInstaller,
    brew: Vec<PathBuf>,
}

impl MacInstaller {
    /// Uses the engine root's `setup` folder and searches it, the owner's `~/.local/bin`,
    /// the engine's own absolute `PATH` folders and the usual Homebrew and system folders.
    /// Input: the engine root. Output: the installer.
    pub fn new(engine_root: &Path) -> Self {
        Self::with_paths(
            engine_root,
            system_folders(&SYSTEM_PATH),
            BREW.iter().map(PathBuf::from).collect(),
        )
    }

    /// Same as `new` with the system folders and Homebrew programs given exactly.
    /// Inputs: the engine root, the folders searched after `setup/bin` and the absolute
    /// `brew` paths to try. Output: the installer.
    pub fn with_paths(engine_root: &Path, system: Vec<PathBuf>, brew: Vec<PathBuf>) -> Self {
        Self {
            pinned: PinnedInstaller::with_paths(engine_root, system),
            brew: brew.into_iter().filter(|path| path.is_absolute()).collect(),
        }
    }

    /// Returns the private install folder, `<engine root>/setup`.
    pub fn setup_folder(&self) -> &Path {
        self.pinned.setup_folder()
    }

    /// Finds the first runnable copy of a program on the search folders and its version.
    /// Input: the dependency. Output: its path and version, or none when no copy reports one.
    pub fn locate(&self, dep: Dependency) -> Option<(PathBuf, Version)> {
        self.pinned.locate(dep)
    }

    /// Returns the `PATH` value that puts `setup/bin` first, for programs the engine runs.
    pub fn search_path(&self) -> OsString {
        self.pinned.search_path()
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
        let mut env = self.pinned.env();
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

impl Installer for MacInstaller {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
        self.pinned.find(dep)
    }

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        self.pinned.install_tool(package, version)
    }

    fn program(&self, name: &str) -> Option<PathBuf> {
        self.pinned.program(name)
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        let Pin::Homebrew { formula } = pin else {
            return self.pinned.install(dep, pin);
        };
        private_folder(self.setup_folder())?;
        self.homebrew(formula)?;
        let (_, found) = self
            .locate(dep)
            .ok_or(InstallError("setup_install_failed"))?;
        Ok(found)
    }
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
