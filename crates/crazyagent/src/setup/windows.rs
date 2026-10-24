use super::run::{reported_version, run_bounded};
use super::{Dependency, InstallError, Installer, Pin, Version};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The version Windows reports for tmux: it needs none, because agent terminals run on the
/// built-in ConPTY host.
pub const CONPTY: &str = "conpty";
/// uv's package id in the winget community source.
pub const UV_WINGET_ID: &str = "astral-sh.uv";

const INSTALL_LIMIT: Duration = Duration::from_secs(15 * 60);
const VERSION_LIMIT: Duration = Duration::from_secs(10);
const NAME_BYTES_MAX: usize = 64;

/// The winget command that installs exactly uv `version` for the owner only, with no
/// prompt and no administrator rights. winget checks the installer's SHA-256 against its
/// manifest before it runs it.
pub fn uv_command(version: &Version) -> Command {
    let mut command = Command::new("winget");
    command.args([
        "install",
        "--id",
        UV_WINGET_ID,
        "--exact",
        "--version",
        version.as_str(),
        "--source",
        "winget",
        "--scope",
        "user",
        "--silent",
        "--disable-interactivity",
        "--accept-package-agreements",
        "--accept-source-agreements",
    ]);
    command
}

/// The Windows installer. tmux reports as found with version `conpty`, because the
/// built-in ConPTY host replaces it. uv comes from winget at the pinned version in the
/// owner's user scope; the release pin's URL and checksum are not used, since winget checks
/// its own manifest checksum. Graphify comes from `uv tool install` into the app data folder.
#[derive(Debug, Clone)]
pub struct WindowsInstaller {
    app_data: PathBuf,
    local_app_data: PathBuf,
}

impl WindowsInstaller {
    /// Input: the app's private data folder, which holds Graphify and its Python, and the
    /// owner's `%LOCALAPPDATA%`, where winget links uv. Output: the installer, or
    /// `setup_store_unavailable` when either path is relative.
    pub fn new(app_data: &Path, local_app_data: &Path) -> Result<Self, InstallError> {
        if !app_data.is_absolute() || !local_app_data.is_absolute() {
            return Err(InstallError("setup_store_unavailable"));
        }
        Ok(Self {
            app_data: app_data.to_path_buf(),
            local_app_data: local_app_data.to_path_buf(),
        })
    }

    /// The `uv tool install` command for Graphify's pinned package, with uv's tools, links
    /// and Python kept in the app data folder. Input: uv, the package, its version and the
    /// Python version. Output: the command, or `setup_pin_invalid` for a package or Python
    /// name that is empty, too long or could read as a flag.
    pub fn graphify_command(
        &self,
        uv: &Path,
        package: &str,
        version: &Version,
        python: &str,
    ) -> Result<Command, InstallError> {
        let plain = |text: &str| {
            !text.is_empty()
                && text.len() <= NAME_BYTES_MAX
                && text
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        if !plain(package) || !plain(python) {
            return Err(InstallError("setup_pin_invalid"));
        }
        let data = self.app_data.join("uv");
        let mut command = Command::new(uv);
        command
            .arg("tool")
            .arg("install")
            .arg(format!("{package}=={}", version.as_str()))
            .args(["--python", python])
            .env("UV_TOOL_DIR", data.join("tools"))
            .env("UV_TOOL_BIN_DIR", data.join("bin"))
            .env("UV_PYTHON_INSTALL_DIR", data.join("python"))
            .env("UV_NO_PROGRESS", "1");
        Ok(command)
    }

    fn uv(&self) -> Option<PathBuf> {
        let linked = self
            .local_app_data
            .join("Microsoft")
            .join("WinGet")
            .join("Links")
            .join("uv.exe");
        std::iter::once(linked)
            .chain(
                std::env::var_os("PATH")
                    .iter()
                    .flat_map(std::env::split_paths)
                    .map(|dir| dir.join("uv.exe")),
            )
            .find(|path| path.is_file())
    }

    fn graphify(&self) -> PathBuf {
        self.app_data.join("uv").join("bin").join("graphify.exe")
    }
}

fn version_of(program: &Path) -> Result<Option<Version>, InstallError> {
    let report = run_bounded(Command::new(program).arg("--version"), VERSION_LIMIT, true)?;
    reported_version(&report).map(Some)
}

impl Installer for WindowsInstaller {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
        match dep {
            Dependency::Tmux => Version::parse(CONPTY).map(Some),
            Dependency::Uv => match self.uv() {
                Some(uv) => version_of(&uv),
                None => Ok(None),
            },
            Dependency::Graphify => {
                let graphify = self.graphify();
                if graphify.is_file() {
                    version_of(&graphify)
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        let (mut command, wanted) = match (dep, pin) {
            (Dependency::Tmux, _) => return Version::parse(CONPTY),
            (Dependency::Uv, Pin::Release { version, .. }) => (uv_command(version), version),
            (
                Dependency::Graphify,
                Pin::UvTool {
                    package,
                    version,
                    python,
                },
            ) => {
                let uv = self.uv().ok_or(InstallError("setup_uv_missing"))?;
                (
                    self.graphify_command(&uv, package, version, python)?,
                    version,
                )
            }
            _ => return Err(InstallError("setup_pin_invalid")),
        };
        run_bounded(&mut command, INSTALL_LIMIT, false)?;
        let found = self
            .find(dep)?
            .ok_or(InstallError("setup_version_unreadable"))?;
        if &found != wanted {
            return Err(InstallError("setup_version_mismatch"));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn windows_needs_no_tmux_and_pins_uv_and_graphify_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let installer = WindowsInstaller::new(&app, &temp.path().join("local")).unwrap();
        let conpty = Version::parse(CONPTY).unwrap();
        assert_eq!(installer.find(Dependency::Tmux), Ok(Some(conpty.clone())));
        let brew = Pin::Homebrew {
            formula: "tmux".into(),
        };
        assert_eq!(installer.install(Dependency::Tmux, &brew), Ok(conpty));
        assert_eq!(installer.find(Dependency::Graphify), Ok(None));
        assert_eq!(
            installer.install(Dependency::Uv, &brew),
            Err(InstallError("setup_pin_invalid"))
        );
        let uv_version = Version::parse("0.9.5").unwrap();
        let uv = uv_command(&uv_version);
        assert_eq!(uv.get_program(), "winget");
        assert!(uv.get_args().eq([
            "install",
            "--id",
            "astral-sh.uv",
            "--exact",
            "--version",
            "0.9.5",
            "--source",
            "winget",
            "--scope",
            "user",
            "--silent",
            "--disable-interactivity",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ]));
        let uv_path = Path::new(r"C:\uv\uv.exe");
        let graphify_version = Version::parse("0.9.9").unwrap();
        let graphify = installer
            .graphify_command(uv_path, "graphifyy", &graphify_version, "3.12")
            .unwrap();
        assert_eq!(graphify.get_program(), uv_path.as_os_str());
        assert!(graphify.get_args().eq([
            "tool",
            "install",
            "graphifyy==0.9.9",
            "--python",
            "3.12"
        ]));
        let envs: Vec<_> = graphify.get_envs().collect();
        let bin = app.join("uv").join("bin");
        assert!(envs.contains(&(OsStr::new("UV_TOOL_BIN_DIR"), Some(bin.as_os_str()))));
        for (package, python) in [
            ("graph ify", "3.12"),
            ("graphifyy", "3.12 --index-url x"),
            ("-e", "3.12"),
            ("", "3.12"),
        ] {
            assert_eq!(
                installer
                    .graphify_command(uv_path, package, &graphify_version, python)
                    .err(),
                Some(InstallError("setup_pin_invalid"))
            );
        }
        assert_eq!(
            WindowsInstaller::new(Path::new("relative"), temp.path()).err(),
            Some(InstallError("setup_store_unavailable"))
        );
    }
}
