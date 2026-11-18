use super::WindowsSandbox;
use crate::sandbox::{ProjectSandbox, SandboxError};
#[cfg(test)]
use crate::sandbox::{SandboxNetwork, canonical};
#[cfg(test)]
use std::ffi::OsStr;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folder in the engine's data root that holds, for each project, owner git's home, its
/// temp folder and its boundary profile.
pub const OWNER_GIT_FOLDER: &str = "owner-git";

#[cfg(test)]
const GLOBAL_CONFIG: &str = "GIT_CONFIG_GLOBAL";

/// Where owner git keeps its files for one project: its home and temp folder, the only
/// folders it writes beside the project, and the boundary profile its helper reads, which
/// lies outside both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerGitPlaces {
    pub home: PathBuf,
    pub tmp: PathBuf,
    pub profile: PathBuf,
}

/// The places owner git uses for `project` under `folder` (red: not built yet).
pub fn places(_folder: &Path, _project: &Path) -> OwnerGitPlaces {
    todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: places")
}

/// Git for Windows' install folder for its program (red: not built yet).
pub fn install_root(_git: &Path) -> Option<&Path> {
    todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: install_root")
}

/// The policy owner git runs under for one project (red: not built yet).
pub fn policy(
    _project: &Path,
    _git: &Path,
    _places: &OwnerGitPlaces,
) -> Result<ProjectSandbox, SandboxError> {
    todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: policy")
}

/// The owner's git for one project, run inside an AppContainer of its own (red: not built yet).
#[derive(Debug)]
pub struct OwnerGit {
    _boundary: WindowsSandbox,
}

impl OwnerGit {
    /// Writes the project's boundary profile (red: not built yet).
    pub fn install(
        _folder: &Path,
        _project: &Path,
        _git: &Path,
        _helper: &Path,
        _grants: &Path,
    ) -> Result<Self, SandboxError> {
        todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: install")
    }

    /// The installed boundary profile (red: not built yet).
    pub fn profile(&self) -> &Path {
        todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: profile")
    }

    /// Wraps one git command built for the project (red: not built yet).
    pub fn wrap(&self, _git: &Command) -> Result<Command, SandboxError> {
        todo!("V244-WIN-GIT-UNC-SIBLINGS slice 2: wrap")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::windows::{GRANTS_FOLDER, container_name};
    use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, check_container};
    use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};

    struct Made {
        _temp: tempfile::TempDir,
        root: PathBuf,
        project: PathBuf,
        folder: PathBuf,
        git: PathBuf,
        grants: PathBuf,
    }

    /// A project, an owner-git folder, a grants folder and a stand-in for Git for Windows'
    /// `cmd\git.exe`, all in one temp folder. Output: the folders.
    fn made() -> Made {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let (project, folder) = (root.join("project"), root.join(OWNER_GIT_FOLDER));
        let (git, grants) = (
            root.join("Git").join("cmd").join("git.exe"),
            root.join(GRANTS_FOLDER),
        );
        for made in [&project, git.parent().unwrap(), &grants] {
            fs::create_dir_all(made).unwrap();
        }
        fs::write(&git, "").unwrap();
        Made {
            _temp: temp,
            root,
            project,
            folder,
            git,
            grants,
        }
    }

    fn installed(made: &Made, project: &Path) -> OwnerGit {
        let helper = std::env::current_exe().unwrap();
        OwnerGit::install(&made.folder, project, &made.git, &helper, &made.grants).unwrap()
    }

    #[test]
    fn git_for_windows_install_folders_are_found_from_its_programs_only() {
        let root = Path::new(r"C:\Programs\Git");
        for program in [
            r"C:\Programs\Git\cmd\git.exe",
            r"C:\Programs\Git\bin\git.exe",
            r"C:\Programs\Git\CMD\git.exe",
            r"C:\Programs\Git\mingw64\bin\git.exe",
            r"C:\Programs\Git\clangarm64\bin\git.exe",
        ] {
            assert_eq!(install_root(Path::new(program)), Some(root), "{program}");
        }
        for program in [
            r"C:\tools\git.exe",
            r"C:\scoop\shims\git.exe",
            r"C:\Programs\Git\mingw64\libexec\git-core\git.exe",
            r"cmd\git.exe",
            r"C:\cmd\git.exe",
        ] {
            assert_eq!(install_root(Path::new(program)), None, "{program}");
        }
    }

    #[test]
    fn owner_git_has_no_capability_writes_only_the_project_and_its_folders_and_is_no_agents_container()
     {
        let made = made();
        let owner = installed(&made, &made.project);
        let profile = BoundaryProfile::load(owner.profile()).unwrap();
        let folder = canonical(&made.folder).unwrap();
        let places = places(&folder, &made.project);
        assert_eq!(owner.profile(), places.profile);
        assert!(
            profile.capabilities.is_empty(),
            "{:?}",
            profile.capabilities
        );
        assert_eq!(
            profile.write_roots,
            [&made.project, &places.home, &places.tmp]
                .map(|path| path.to_str().unwrap().to_owned())
        );
        assert_eq!(
            profile.read_roots,
            [made.root.join("Git").to_str().unwrap().to_owned()]
        );
        assert!(
            profile
                .guarded
                .iter()
                .all(|item| item.guard == Guard::Hidden),
            "{:?}",
            profile.guarded
        );
        assert_eq!(check_container(&profile.container), Ok(()));
        let (agent_home, agent_tmp) = (made.root.join("agent-home"), made.root.join("agent-tmp"));
        fs::create_dir_all(&agent_home).unwrap();
        fs::create_dir_all(&agent_tmp).unwrap();
        let agent =
            ProjectSandbox::new(&made.project, &agent_home, &agent_tmp, SandboxNetwork::Open)
                .unwrap();
        assert_ne!(container_name(&agent), profile.container);
        let again = installed(&made, &made.project);
        assert_eq!(again.profile(), owner.profile());
        assert_eq!(
            BoundaryProfile::load(again.profile()).unwrap().container,
            profile.container
        );
        let other = made.root.join("other");
        fs::create_dir_all(&other).unwrap();
        let elsewhere = installed(&made, &other);
        assert_ne!(elsewhere.profile(), owner.profile());
        assert_ne!(
            BoundaryProfile::load(elsewhere.profile())
                .unwrap()
                .container,
            profile.container
        );
    }

    #[test]
    fn owner_git_refuses_a_git_outside_a_git_for_windows_install_before_any_profile() {
        let made = made();
        let loose = made.root.join("tools").join("git.exe");
        fs::create_dir_all(loose.parent().unwrap()).unwrap();
        fs::write(&loose, "").unwrap();
        let helper = std::env::current_exe().unwrap();
        match OwnerGit::install(&made.folder, &made.project, &loose, &helper, &made.grants) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(reason.contains("Git for Windows"), "{reason}")
            }
            other => panic!("expected the install folder refusal, got {other:?}"),
        }
        let folder = canonical(&made.folder).unwrap();
        assert!(!places(&folder, &made.project).profile.exists());
    }

    #[test]
    fn a_wrapped_git_starts_through_the_helper_in_its_own_home_with_no_global_config() {
        let made = made();
        let owner = installed(&made, &made.project);
        let folder = canonical(&made.folder).unwrap();
        let places = places(&folder, &made.project);
        let mut git = Command::new(&made.git);
        git.env("LC_ALL", "C")
            .env(GLOBAL_CONFIG, crate::github::NULL_DEVICE)
            .env("GIT_COMMON_DIR", made.project.join(".git"))
            .envs(crate::research::runner::GIT_SAFE_ENV)
            .env("HOME", made.root.join("publish-home"))
            .env("SystemRoot", r"C:\Elsewhere")
            .env_remove("GIT_DIR")
            .current_dir(&made.project)
            .args(["ls-files", "-z", "--cached"]);
        let wrapped = owner.wrap(&git).unwrap();
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        assert_eq!(
            args,
            [
                OsStr::new(PROFILE_FLAG),
                owner.profile().as_os_str(),
                OsStr::new(SEPARATOR),
                made.git.as_os_str(),
                OsStr::new("ls-files"),
                OsStr::new("-z"),
                OsStr::new("--cached"),
            ]
        );
        assert_eq!(wrapped.get_current_dir(), Some(made.project.as_path()));
        let env = |name: &str| {
            wrapped
                .get_envs()
                .find(|(known, _)| *known == OsStr::new(name))
                .and_then(|(_, value)| value)
        };
        assert_eq!(env("HOME"), Some(places.home.as_os_str()));
        assert_eq!(env("USERPROFILE"), Some(places.home.as_os_str()));
        assert_eq!(env("TEMP"), Some(places.tmp.as_os_str()));
        assert_eq!(env(GLOBAL_CONFIG), Some(OsStr::new("/dev/null")));
        assert_eq!(
            env("GIT_COMMON_DIR"),
            Some(made.project.join(".git").as_os_str())
        );
        assert_eq!(env("GIT_NO_LAZY_FETCH"), Some(OsStr::new("1")));
        assert_eq!(env("LC_ALL"), Some(OsStr::new("C")));
        assert_ne!(env("SystemRoot"), Some(OsStr::new(r"C:\Elsewhere")));
        assert_eq!(env("GIT_DIR"), None);
    }

    #[test]
    fn a_wrapped_git_refuses_another_program_a_global_config_or_a_folder_outside_the_project() {
        let made = made();
        let owner = installed(&made, &made.project);
        let refused = |git: &Command, why: &str| {
            assert!(
                matches!(owner.wrap(git), Err(SandboxError::Invalid(_))),
                "{why}"
            );
        };
        let mut other = Command::new(made.root.join("Git").join("bin").join("git.exe"));
        other.current_dir(&made.project);
        refused(&other, "another program");
        let mut config = Command::new(&made.git);
        config
            .current_dir(&made.project)
            .env(GLOBAL_CONFIG, made.root.join("gitconfig"));
        refused(&config, "a global config file");
        let mut outside = Command::new(&made.git);
        outside.current_dir(&made.root);
        refused(&outside, "a folder outside the project");
        let mut plain = Command::new(&made.git);
        plain.current_dir(&made.project);
        assert!(owner.wrap(&plain).is_ok());
    }
}
