use super::pinned::{PinnedInstaller, runnable, system_folders};
use super::process::{private_folder, run};
use super::{Dependency, InstallError, Installer, Pin, Version};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use super::{GRAPHIFY_PACKAGE, GRAPHIFY_VERSION, UV_VERSION};

const SYSTEM_PATH: [&str; 2] = ["/usr/bin", "/bin"];
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
    /// Uses the engine root's `setup` folder and finds programs wherever the tool resolver
    /// looks for this engine ([`crate::tools::Places::from_environment`]), the answer every
    /// launch uses. Homebrew's folders and its `brew` come from the resolver's own list
    /// ([`crate::tools::homebrew_folders`]), so the clean-Mac gate's remap covers them; the
    /// programs setup starts search `setup/bin`, `~/.local/bin`, the engine's `PATH`,
    /// Homebrew and the system folders. Input: the engine root. Output: the installer.
    pub fn new(engine_root: &Path) -> Self {
        let homebrew = crate::tools::homebrew_folders();
        let brew: Vec<PathBuf> = homebrew.iter().map(|folder| folder.join("brew")).collect();
        let mut fixed = homebrew;
        fixed.extend(SYSTEM_PATH.iter().map(PathBuf::from));
        Self {
            pinned: PinnedInstaller::with_places(
                engine_root,
                system_folders(fixed),
                crate::tools::Places::from_environment(Some(engine_root)),
            ),
            brew: brew.into_iter().filter(|path| path.is_absolute()).collect(),
        }
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

    fn lookup(&self, dep: Dependency) -> super::Lookup {
        self.pinned.lookup(dep)
    }

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        self.pinned.install_tool(package, version)
    }

    fn program(&self, name: &str) -> Option<PathBuf> {
        self.pinned.program(name)
    }

    fn lookup_program(&self, name: &str) -> Result<PathBuf, crate::tools::Missing> {
        self.pinned.lookup_program(name)
    }

    fn fix_target(
        &self,
        missing: &crate::tools::Missing,
    ) -> Option<crate::tools::permission_fix::Target> {
        self.pinned.fix_target(missing)
    }

    fn fix_permissions(
        &self,
        dep: Dependency,
        confirmed: &str,
    ) -> Result<crate::tools::permission_fix::Report, &'static str> {
        self.pinned.fix_permissions(dep, confirmed)
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
