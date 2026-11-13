use super::run::run_bounded;
use super::{Dependency, InstallError, Installer, Pin, Version};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where Homebrew on Linux keeps `brew`. It is never looked up through `PATH`.
pub const LINUXBREW: &str = "/home/linuxbrew/.linuxbrew/bin/brew";
/// The fixed path of apt's installer on Debian and Ubuntu.
pub const APT_GET: &str = "/usr/bin/apt-get";
/// The fixed path of dnf on Fedora and RHEL.
pub const DNF: &str = "/usr/bin/dnf";
/// The folders searched for uv and Graphify after `setup/bin`, `~/.local/bin` and `PATH`.
pub const SYSTEM_PATH: [&str; 4] = [
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/home/linuxbrew/.linuxbrew/bin",
];

const INSTALL_LIMIT: Duration = Duration::from_secs(15 * 60);
const FORMULA_BYTES_MAX: usize = 64;

/// A distribution package manager setup may use for tmux, only when the engine already runs
/// as root. Setup never calls sudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Dnf,
}

impl PackageManager {
    /// The package manager at its fixed path on this machine, apt before dnf, or none.
    pub fn detect() -> Option<Self> {
        if Path::new(APT_GET).is_file() {
            Some(Self::Apt)
        } else if Path::new(DNF).is_file() {
            Some(Self::Dnf)
        } else {
            None
        }
    }
}

/// The command that installs tmux's package. Input: the pinned formula or package name,
/// Homebrew's `brew` when present, whether the engine runs as root, and the distribution's
/// package manager. Output: `brew install` when Homebrew is present; otherwise apt or dnf,
/// only as root; `setup_homebrew_missing` when neither applies, so the owner installs tmux;
/// `setup_pin_invalid` for a name that is empty, too long or could read as a flag.
pub fn tmux_command(
    formula: &str,
    brew: Option<&Path>,
    root: bool,
    manager: Option<PackageManager>,
) -> Result<Command, InstallError> {
    let plain = !formula.is_empty()
        && formula.len() <= FORMULA_BYTES_MAX
        && formula
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && formula
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'@' | b'+' | b'_'));
    if !plain {
        return Err(InstallError("setup_pin_invalid"));
    }
    if let Some(brew) = brew {
        let mut command = Command::new(brew);
        command
            .args(["install", "--formula", formula])
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("HOMEBREW_NO_ANALYTICS", "1");
        return Ok(command);
    }
    match (root, manager) {
        (true, Some(PackageManager::Apt)) => {
            let mut command = Command::new(APT_GET);
            command
                .args(["install", "-y", "--no-install-recommends", formula])
                .env("DEBIAN_FRONTEND", "noninteractive");
            Ok(command)
        }
        (true, Some(PackageManager::Dnf)) => {
            let mut command = Command::new(DNF);
            command.args(["install", "-y", formula]);
            Ok(command)
        }
        _ => Err(InstallError("setup_homebrew_missing")),
    }
}

/// The Linux installer. tmux comes from Homebrew on Linux, or from apt or dnf when the
/// engine already runs as root; setup never calls sudo. uv and Graphify go through the
/// shared pinned installer, the same checksum-verified path as macOS.
pub struct LinuxInstaller {
    common: Box<dyn Installer>,
}

impl LinuxInstaller {
    /// Input: the shared installer that installs uv from its checksum-verified release and
    /// Graphify through `uv tool install`. Output: the Linux installer around it.
    pub fn new(common: Box<dyn Installer>) -> Self {
        Self { common }
    }
}

impl Installer for LinuxInstaller {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
        self.common.find(dep)
    }

    fn lookup(&self, dep: Dependency) -> super::Lookup {
        self.common.lookup(dep)
    }

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        self.common.install_tool(package, version)
    }

    fn program(&self, name: &str) -> Option<PathBuf> {
        self.common.program(name)
    }

    fn lookup_program(&self, name: &str) -> Result<PathBuf, crate::tools::Missing> {
        self.common.lookup_program(name)
    }

    fn fix_target(
        &self,
        missing: &crate::tools::Missing,
    ) -> Option<crate::tools::permission_fix::Target> {
        self.common.fix_target(missing)
    }

    fn fix_permissions(
        &self,
        dep: Dependency,
        confirmed: &str,
    ) -> Result<crate::tools::permission_fix::Report, &'static str> {
        self.common.fix_permissions(dep, confirmed)
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        if dep != Dependency::Tmux {
            return self.common.install(dep, pin);
        }
        let Pin::Homebrew { formula } = pin else {
            return Err(InstallError("setup_pin_invalid"));
        };
        let brew = PathBuf::from(LINUXBREW);
        let mut command = tmux_command(
            formula,
            brew.is_file().then_some(brew.as_path()),
            rustix::process::geteuid().is_root(),
            PackageManager::detect(),
        )?;
        run_bounded(&mut command, INSTALL_LIMIT, false)?;
        self.common
            .find(Dependency::Tmux)?
            .ok_or(InstallError("setup_version_unreadable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::pinned::PinnedInstaller;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn uv_graphify_tools_and_programs_go_through_the_shared_installer() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let shared = PinnedInstaller::with_paths(root.path(), vec![]);
        let installer = LinuxInstaller::new(Box::new(shared));
        assert_eq!(installer.find(Dependency::Uv), Ok(None));
        let uv = root.path().join("setup/bin/uv");
        crate::setup::process::private_folder(uv.parent().unwrap()).unwrap();
        std::fs::write(&uv, "#!/bin/sh\necho \"uv 0.13.5 (fixture)\"\n").unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            installer.find(Dependency::Uv),
            Ok(Some(Version::parse("0.13.5").unwrap()))
        );
        assert_eq!(installer.program("uv"), Some(uv.clone()));
        assert_eq!(installer.program("../uv"), None);
        assert_eq!(
            installer.install_tool("graphifyy", "0.9.9"),
            Err(InstallError("setup_install_failed"))
        );
        let home = root.path().join("setup/uv/tools/graphifyy");
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        assert!(!root.path().join("setup/bin/graphify").exists());
        assert_eq!(
            installer.install_tool("Graph ify", "0.9.9"),
            Err(InstallError("setup_pin_invalid"))
        );
        let brew = Pin::Homebrew {
            formula: "uv".into(),
        };
        assert_eq!(
            installer.install(Dependency::Uv, &brew),
            Err(InstallError("setup_pin_invalid"))
        );
    }

    #[test]
    fn tmux_installs_through_homebrew_or_a_root_package_manager_and_never_sudo() {
        let brew = Path::new(LINUXBREW);
        let homebrew = tmux_command("tmux", Some(brew), false, Some(PackageManager::Apt)).unwrap();
        assert_eq!(homebrew.get_program(), brew.as_os_str());
        assert!(homebrew.get_args().eq(["install", "--formula", "tmux"]));
        let apt = tmux_command("tmux", None, true, Some(PackageManager::Apt)).unwrap();
        assert_eq!(apt.get_program(), APT_GET);
        assert!(
            apt.get_args()
                .eq(["install", "-y", "--no-install-recommends", "tmux"])
        );
        let dnf = tmux_command("tmux", None, true, Some(PackageManager::Dnf)).unwrap();
        assert_eq!(dnf.get_program(), DNF);
        assert!(dnf.get_args().eq(["install", "-y", "tmux"]));
        for (root, manager) in [
            (false, Some(PackageManager::Apt)),
            (false, Some(PackageManager::Dnf)),
            (false, None),
            (true, None),
        ] {
            assert_eq!(
                tmux_command("tmux", None, root, manager).err(),
                Some(InstallError("setup_homebrew_missing"))
            );
        }
        let long = "t".repeat(FORMULA_BYTES_MAX + 1);
        for formula in ["", "-y", "tmux; reboot", "tmux other", long.as_str()] {
            assert_eq!(
                tmux_command(formula, Some(brew), true, Some(PackageManager::Apt)).err(),
                Some(InstallError("setup_pin_invalid"))
            );
        }
    }

    /// On Linux, setup answers tmux wherever the tool resolver looks, as the live master's
    /// start does ([`crate::terminal::tmux::installed`]): a tmux only in `~/.local/bin`
    /// shows Found with its version, and none anywhere gives the resolver's tmux line.
    #[test]
    fn a_linux_tmux_row_resolves_where_the_live_start_does() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().join("home");
        let local = home.join(".local/bin");
        std::fs::create_dir_all(&local).unwrap();
        let places = crate::tools::Places::new(
            vec![root.path().join("setup/bin"), local.clone()],
            Some(home),
        );
        let shared = PinnedInstaller::with_places(root.path(), vec![], places.clone());
        let installer = LinuxInstaller::new(Box::new(shared));
        let missing = installer.lookup(Dependency::Tmux).unwrap().unwrap_err();
        assert_eq!(missing, crate::tools::resolve("tmux", &places).unwrap_err());
        assert_eq!(missing.line(), crate::tools::TMUX_MISSING);
        assert_eq!(installer.find(Dependency::Tmux), Ok(None));
        let tmux = local.join("tmux");
        std::fs::write(&tmux, "#!/bin/sh\necho 'tmux 3.5a'\n").unwrap();
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            installer.lookup(Dependency::Tmux).unwrap(),
            Ok(crate::setup::Located {
                version: Version::parse("3.5a").unwrap(),
                bundled: false,
                note: None,
            })
        );
        assert_eq!(
            installer.find(Dependency::Tmux),
            Ok(Some(Version::parse("3.5a").unwrap()))
        );
    }

    /// The tool resolver looks in Linux's package folders that are often not on the engine's
    /// PATH, Linuxbrew where setup installs tmux and the NixOS system and default profiles,
    /// before the system folders, so a tmux setup finds there is the one the live start runs.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_resolver_looks_in_linuxbrew_and_the_nixos_profiles() {
        let places = crate::tools::Places::engine(None, None, None, None);
        let folders = places.folders();
        let system = folders
            .iter()
            .position(|folder| folder == Path::new("/usr/bin"))
            .unwrap();
        for folder in [
            "/home/linuxbrew/.linuxbrew/bin",
            "/run/current-system/sw/bin",
            "/nix/var/nix/profiles/default/bin",
        ] {
            let at = folders
                .iter()
                .position(|found| found == Path::new(folder))
                .unwrap_or_else(|| panic!("{folder} is not looked in"));
            assert!(at < system, "{folder} comes after the system folders");
        }
    }

    /// Setup's tmux row and the live master's start ask the same resolver on this machine:
    /// setup finds tmux exactly when [`crate::terminal::tmux::installed`] does and names the
    /// same program, and a missing tmux gives the resolver's tmux line.
    #[test]
    fn setup_finds_tmux_exactly_when_the_live_start_does() {
        let root = tempfile::tempdir().unwrap();
        let places = crate::tools::Places::from_environment(Some(root.path()));
        let shared = PinnedInstaller::with_places(root.path(), vec![], places);
        let installer = LinuxInstaller::new(Box::new(shared));
        let started = crate::terminal::tmux::installed(root.path());
        let found = installer.program("tmux");
        assert_eq!(
            found.map(|entry| entry.canonicalize().unwrap()),
            started
                .as_ref()
                .map(|program| program.canonicalize().unwrap())
        );
        match installer.lookup(Dependency::Tmux) {
            Ok(Ok(_)) => assert!(started.is_some()),
            Ok(Err(missing)) => {
                assert!(started.is_none());
                assert_eq!(missing.line(), crate::tools::TMUX_MISSING);
            }
            Err(code) => assert_eq!(code, InstallError("setup_version_unreadable")),
        }
    }
}
