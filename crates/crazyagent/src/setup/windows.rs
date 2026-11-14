use super::run::{reported_version, run_bounded};
use super::{
    Dependency, InstallError, Installer, LOCK_FILE, Pin, UV_WINGET_ID, Version, keep_only,
    locked_tool_args, tool_lock,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The version Windows reports for tmux: it needs none, because agent terminals run on the
/// built-in ConPTY host.
pub const CONPTY: &str = "conpty";

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
/// built-in ConPTY host replaces it. uv comes from its winget pin, `astral-sh.uv` at the
/// pinned version in the owner's user scope; winget checks its own manifest checksum.
/// Graphify comes from its bundled hash lock, installed into the app data folder.
#[derive(Debug, Clone)]
pub struct WindowsInstaller {
    app_data: PathBuf,
    local_app_data: PathBuf,
    places: crate::tools::Places,
}

impl WindowsInstaller {
    /// Input: the app's private data folder, which holds Graphify and its Python, the
    /// owner's `%LOCALAPPDATA%`, where winget links uv, if Windows set it, and the tool
    /// resolver's places, whose refusals decide the PATH folders setup finds uv in and hands
    /// uv. Output: the installer, `setup_local_app_data_missing` when `%LOCALAPPDATA%` is
    /// unset or relative, or `setup_store_unavailable` when the app data folder is relative.
    pub fn new(
        app_data: &Path,
        local_app_data: Option<&Path>,
        places: crate::tools::Places,
    ) -> Result<Self, InstallError> {
        let Some(local_app_data) = local_app_data.filter(|path| path.is_absolute()) else {
            return Err(InstallError("setup_local_app_data_missing"));
        };
        if !app_data.is_absolute() {
            return Err(InstallError("setup_store_unavailable"));
        }
        Ok(Self {
            app_data: app_data.to_path_buf(),
            local_app_data: local_app_data.to_path_buf(),
            places,
        })
    }

    /// The two uv commands that install Graphify's pinned package from its bundled hash
    /// lock into one fresh environment folder, with uv's cache and Python kept in the app
    /// data folder and no inherited uv settings. Inputs: uv, the environment folder, the
    /// package, its version and the Python version. Output: the commands,
    /// `setup_pin_invalid` for a package or Python name that is empty, too long or could
    /// read as a flag, or `setup_pin_unlocked` when setup ships no lock for that version.
    pub fn graphify_commands(
        &self,
        uv: &Path,
        env: &Path,
        package: &str,
        version: &Version,
        python: &str,
    ) -> Result<[Command; 2], InstallError> {
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
        tool_lock(package, version.as_str()).ok_or(InstallError("setup_pin_unlocked"))?;
        let data = self.app_data.join("uv");
        let venv = env.join("venv");
        let args = locked_tool_args(
            python,
            &venv,
            &venv.join("Scripts").join("python.exe"),
            &env.join(LOCK_FILE),
        );
        Ok(args.map(|args| {
            let mut command = Command::new(uv);
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("UV_") {
                    command.env_remove(name);
                }
            }
            command
                .args(args)
                .env("PATH", self.search_path())
                .env("UV_CACHE_DIR", data.join("cache"))
                .env("UV_PYTHON_INSTALL_DIR", data.join("python"))
                .env("UV_NO_PROGRESS", "1");
            command
        }))
    }

    fn install_graphify(
        &self,
        uv: &Path,
        package: &str,
        version: &Version,
        python: &str,
    ) -> Result<(), InstallError> {
        let store = InstallError("setup_store_unavailable");
        let random = crate::engine::owner::random_id().map_err(|_| store)?;
        let home = self.app_data.join("uv").join("tools").join(package);
        let env = home.join(format!("env-{}", &random[..24]));
        let commands = self.graphify_commands(uv, &env, package, version, python)?;
        let lock =
            tool_lock(package, version.as_str()).ok_or(InstallError("setup_pin_unlocked"))?;
        std::fs::create_dir_all(&env).map_err(|_| store)?;
        let result = (|| {
            std::fs::write(env.join(LOCK_FILE), lock.requirements).map_err(|_| store)?;
            for mut command in commands {
                run_bounded(&mut command, INSTALL_LIMIT, false)?;
            }
            let scripts = env.join("venv").join("Scripts");
            let bin = self.app_data.join("uv").join("bin");
            std::fs::create_dir_all(&bin).map_err(|_| store)?;
            for name in lock.programs {
                let program = scripts.join(format!("{name}.exe"));
                if !program.is_file() {
                    return Err(InstallError("setup_install_failed"));
                }
                let staged = bin.join(format!(".{name}-{}.exe", &random[..24]));
                std::fs::copy(&program, &staged).map_err(|_| store)?;
                std::fs::rename(&staged, bin.join(format!("{name}.exe"))).map_err(|_| {
                    let _ = std::fs::remove_file(&staged);
                    store
                })?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => keep_only(&home, &env),
            Err(_) => {
                let _ = std::fs::remove_dir_all(&env);
            }
        }
        result
    }

    fn uv(&self) -> Option<PathBuf> {
        self.uv_on(std::env::var_os("PATH").as_deref())
    }

    /// The uv setup runs: the owner's WinGet Links copy, else the first `uv.exe` in a PATH
    /// folder the installer's places keep ([`Self::kept`]). Input: the PATH value. Output:
    /// the file, or none.
    fn uv_on(&self, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
        let linked = self
            .local_app_data
            .join("Microsoft")
            .join("WinGet")
            .join("Links")
            .join("uv.exe");
        std::iter::once(linked)
            .chain(self.kept(path).into_iter().map(|dir| dir.join("uv.exe")))
            .find(|path| path.is_file())
    }

    /// The PATH folders the programs setup finds and starts may use: absolute ones the
    /// installer's places keep ([`crate::tools::Places::kept_for_children`]), so a relative or
    /// empty entry, or a folder agents can write, never gives uv, its Python or what uv runs.
    /// Input: the PATH value. Output: the folders in PATH order.
    fn kept(&self, path: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
        let kept: Vec<PathBuf> = path
            .into_iter()
            .flat_map(std::env::split_paths)
            .filter(|dir| dir.is_absolute() && self.places.kept_for_children(dir))
            .collect();
        assert!(kept.iter().all(|dir| dir.is_absolute()));
        kept
    }

    /// The PATH uv gets: the engine's PATH with only the folders setup keeps ([`Self::kept`]).
    /// Output: the value; empty when the kept folders cannot be joined.
    fn search_path(&self) -> std::ffi::OsString {
        let kept = self.kept(std::env::var_os("PATH").as_deref());
        std::env::join_paths(kept).unwrap_or_default()
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
            Dependency::Node => Err(InstallError(super::PLATFORM_UNSUPPORTED)),
        }
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        let wanted = match (dep, pin) {
            (Dependency::Tmux, _) => return Version::parse(CONPTY),
            (Dependency::Uv, Pin::Winget { id, version }) if id == UV_WINGET_ID => {
                run_bounded(&mut uv_command(version), INSTALL_LIMIT, false)?;
                version
            }
            (
                Dependency::Graphify,
                Pin::UvTool {
                    package,
                    version,
                    python,
                },
            ) => {
                let uv = self.uv().ok_or(InstallError("setup_uv_missing"))?;
                self.install_graphify(&uv, package, version, python)?;
                version
            }
            _ => return Err(InstallError("setup_pin_invalid")),
        };
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

    /// Places with no folders and no HOME, whose refusals are the ones every place has, so a
    /// row's temp folders are kept and its project folders are not. Output: the places.
    fn places() -> crate::tools::Places {
        crate::tools::Places::new(Vec::new(), None)
    }

    /// RV-DS241-95: setup on Windows finds uv only in an absolute PATH folder the tool
    /// resolver's places keep, never in a relative entry or a project folder agents can
    /// write, so the background status check never runs a planted uv.exe.
    #[test]
    fn windows_setup_never_finds_uv_in_a_relative_or_agent_writable_path_folder() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let project = temp.path().join("proj");
        std::fs::create_dir_all(project.join(".crazyagent")).unwrap();
        let planted = project.join("bin");
        let owner = temp.path().join("owner");
        for folder in [&planted, &owner] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("uv.exe"), b"uv").unwrap();
        }
        let installer =
            WindowsInstaller::new(&temp.path().join("app"), Some(&local), places()).unwrap();
        let path = std::env::join_paths([
            PathBuf::from("relative-bin"),
            planted.clone(),
            owner.clone(),
        ])
        .unwrap();
        assert_eq!(installer.kept(Some(path.as_os_str())), vec![owner.clone()]);
        assert_eq!(
            installer.uv_on(Some(path.as_os_str())),
            Some(owner.join("uv.exe"))
        );
    }

    #[test]
    fn windows_needs_no_tmux_and_pins_uv_and_graphify_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let local = temp.path().join("local");
        let installer = WindowsInstaller::new(&app, Some(&local), places()).unwrap();
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
        let env = app.join("uv").join("tools").join("graphifyy").join("env-1");
        let [venv, pip] = installer
            .graphify_commands(uv_path, &env, "graphifyy", &graphify_version, "3.12")
            .unwrap();
        let venv_folder = env.join("venv");
        let venv_python = venv_folder.join("Scripts").join("python.exe");
        let lock_file = env.join("requirements.txt");
        let [venv_args, pip_args] =
            crate::setup::locked_tool_args("3.12", &venv_folder, &venv_python, &lock_file);
        for (command, args) in [(&venv, &venv_args), (&pip, &pip_args)] {
            assert_eq!(command.get_program(), uv_path.as_os_str());
            assert!(command.get_args().eq(args.iter()));
            let envs: Vec<_> = command.get_envs().collect();
            let python = app.join("uv").join("python");
            assert!(envs.contains(&(
                OsStr::new("UV_PYTHON_INSTALL_DIR"),
                Some(python.as_os_str())
            )));
        }
        assert!(pip.get_args().any(|arg| arg == "--require-hashes"));
        assert_eq!(
            installer
                .graphify_commands(
                    uv_path,
                    &env,
                    "graphifyy",
                    &Version::parse("0.9.8").unwrap(),
                    "3.12"
                )
                .err(),
            Some(InstallError("setup_pin_unlocked"))
        );
        for (package, python) in [
            ("graph ify", "3.12"),
            ("graphifyy", "3.12 --index-url x"),
            ("-e", "3.12"),
            ("", "3.12"),
        ] {
            assert_eq!(
                installer
                    .graphify_commands(uv_path, &env, package, &graphify_version, python)
                    .err(),
                Some(InstallError("setup_pin_invalid"))
            );
        }
        assert_eq!(
            WindowsInstaller::new(Path::new("relative"), Some(temp.path()), places()).err(),
            Some(InstallError("setup_store_unavailable"))
        );
    }

    #[test]
    fn windows_refuses_plainly_without_an_absolute_local_app_data_folder() {
        let temp = tempfile::tempdir().unwrap();
        for local in [None, Some(Path::new("relative"))] {
            assert_eq!(
                WindowsInstaller::new(temp.path(), local, places()).err(),
                Some(InstallError("setup_local_app_data_missing"))
            );
        }
        let sentence = crate::setup::plain_reason("setup_local_app_data_missing");
        assert!(sentence.contains("local app data"), "{sentence}");
        assert!(WindowsInstaller::new(temp.path(), Some(temp.path()), places()).is_ok());
    }

    #[test]
    fn uv_installs_only_from_its_own_winget_package() {
        let temp = tempfile::tempdir().unwrap();
        let installer = WindowsInstaller::new(temp.path(), Some(temp.path()), places()).unwrap();
        let version = Version::parse("0.13.5").unwrap();
        for (dep, id) in [
            (Dependency::Uv, "someone.uv"),
            (Dependency::Uv, "astral-sh.uv --override x"),
            (Dependency::Graphify, UV_WINGET_ID),
        ] {
            let pin = Pin::Winget {
                id: id.to_owned(),
                version: version.clone(),
            };
            assert_eq!(
                installer.install(dep, &pin),
                Err(InstallError("setup_pin_invalid"))
            );
        }
    }
}
