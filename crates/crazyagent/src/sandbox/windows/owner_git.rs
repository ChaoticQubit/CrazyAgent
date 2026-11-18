use super::{WindowsSandbox, container_for};
use crate::sandbox::{
    AGENT_HOME_ENV, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, canonical, invalid,
};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folder in the engine's data root that holds, for each project, owner git's home, its
/// temp folder and its boundary profile.
pub const OWNER_GIT_FOLDER: &str = "owner-git";

const PROGRAM_FOLDERS: [&str; 2] = ["cmd", "bin"];
const PLATFORM_FOLDERS: [&str; 3] = ["mingw64", "clangarm64", "mingw32"];
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

/// The places owner git uses for `project` under `folder`: a folder named from the project
/// alone, so every git of that project shares one container and its grants, holding `home`
/// and `tmp`, and beside it the profile `<name>.sb`. Inputs: the canonical owner-git folder
/// and the canonical project. Output: the places.
pub fn places(folder: &Path, project: &Path) -> OwnerGitPlaces {
    let name = container_for(project, folder);
    let own = folder.join(&name);
    let places = OwnerGitPlaces {
        home: own.join("home"),
        tmp: own.join("tmp"),
        profile: folder.join(format!("{name}.sb")),
    };
    assert!(!places.profile.starts_with(&own));
    places
}

/// Git for Windows' install folder for its program: the folder above `cmd` or `bin`, or above
/// a platform folder's `bin` such as `mingw64\bin`, so the program the launcher starts and its
/// libraries are readable too. Input: git's absolute program. Output: the folder, or none for a
/// program outside those layouts.
pub fn install_root(git: &Path) -> Option<&Path> {
    let named = |path: &Path, names: &[&str]| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| names.iter().any(|known| name.eq_ignore_ascii_case(known)))
    };
    let folder = git.parent()?;
    if !git.is_absolute() || !named(folder, &PROGRAM_FOLDERS) {
        return None;
    }
    let above = folder.parent()?;
    let root = if named(folder, &["bin"]) && named(above, &PLATFORM_FOLDERS) {
        above.parent()?
    } else {
        above
    };
    root.file_name()?;
    assert!(git.starts_with(root) && root != folder);
    Some(root)
}

/// The policy owner git runs under for one project: no network, the project and its own home
/// and temp folder writable, nothing guarded, since publish's `git init` makes `.git` there, and
/// Git for Windows' install folder readable once it holds the markers ([`marked`]) and neither
/// is nor holds the project or owner git's folder (the owner home is `allow_read`'s check).
/// Inputs: the canonical project, git's program and the places. Output: the policy, or why not.
pub fn policy(
    project: &Path,
    git: &Path,
    places: &OwnerGitPlaces,
) -> Result<ProjectSandbox, SandboxError> {
    let root = install_root(git)
        .filter(|root| marked(root))
        .ok_or_else(|| invalid("git is not in a Git for Windows install folder"))?;
    let root = canonical(root)?;
    if project.starts_with(&root) || places.profile.starts_with(&root) {
        return Err(invalid(
            "Git for Windows' install folder holds the project or owner git's own folder",
        ));
    }
    ProjectSandbox::new(
        project,
        &places.home,
        &places.tmp,
        SandboxNetwork::Only(Vec::new()),
    )?
    .as_job_folder()
    .allow_read(&root)
}

/// True when a folder holds Git for Windows' markers, as every installer and MinGit lay them
/// out: `cmd\git.exe` and a platform folder such as `mingw64`, so a `git.exe` that sits in any
/// folder's `cmd` or `bin` never makes that whole folder readable (rv-sec3 #628 F1). Input:
/// the folder above the program. Output: the answer.
fn marked(root: &Path) -> bool {
    root.join("cmd").join("git.exe").is_file()
        && PLATFORM_FOLDERS
            .iter()
            .any(|platform| root.join(platform).is_dir())
}

/// The owner's git for one project, run inside an AppContainer of its own with no capability
/// (owner option 1, V244-WIN-GIT-UNC-SIBLINGS), so a `.git` that points git at a share or a
/// folder outside the project reaches nothing. Every command reuses the profile written once.
#[derive(Debug)]
pub struct OwnerGit {
    boundary: WindowsSandbox,
    git: PathBuf,
}

impl OwnerGit {
    /// Makes the project's places under `folder` and writes its boundary profile there for
    /// `git`, bound to `helper` and recording grants in `grants`. Inputs: the owner-git folder,
    /// the project, git's absolute program, the helper and the grants folder. Output: owner
    /// git, or the refusal: a project or git no boundary can hold, or a missing helper.
    pub fn install(
        folder: &Path,
        project: &Path,
        git: &Path,
        helper: &Path,
        grants: &Path,
    ) -> Result<Self, SandboxError> {
        fs::create_dir_all(folder)?;
        let folder = canonical(folder)?;
        let project = canonical(project)?;
        let places = places(&folder, &project);
        fs::create_dir_all(&places.home)?;
        fs::create_dir_all(&places.tmp)?;
        let policy = policy(&project, git, &places)?;
        let boundary = WindowsSandbox::install(policy, helper, &places.profile, grants)?;
        assert_eq!(boundary.profile(), places.profile);
        Ok(Self {
            boundary,
            git: git.to_path_buf(),
        })
    }

    /// The installed boundary profile every wrapped command names.
    pub fn profile(&self) -> &Path {
        self.boundary.profile()
    }

    /// Wraps one git command built for the project: the same program, arguments, folder and
    /// variables but `GIT_CONFIG_GLOBAL`, which the boundary sets to the null device for every
    /// process ([`AGENT_HOME_ENV`]), so the command must name the null device too. The wrapped
    /// command has default stdio: the caller sets it again. Input: the command. Output: the
    /// wrapped command, or `Invalid` for another program, global config or starting folder.
    pub fn wrap(&self, git: &Command) -> Result<Command, SandboxError> {
        if Path::new(git.get_program()) != self.git {
            return Err(invalid(
                "owner git runs only the git its boundary was installed for",
            ));
        }
        let mut bare = Command::new(&self.git);
        bare.args(git.get_args());
        if let Some(folder) = git.get_current_dir() {
            bare.current_dir(folder);
        }
        for (name, value) in git.get_envs() {
            if name.eq_ignore_ascii_case(GLOBAL_CONFIG) {
                if value != Some(OsStr::new(crate::github::NULL_DEVICE)) {
                    return Err(invalid("owner git reads no global git config"));
                }
                continue;
            }
            match value {
                Some(value) => bare.env(name, value),
                None => bare.env_remove(name),
            };
        }
        let wrapped = self.boundary.wrap(&bare)?;
        assert!(AGENT_HOME_ENV.contains(&(GLOBAL_CONFIG, "/dev/null")));
        assert!(
            wrapped.get_envs().any(
                |(name, value)| name == GLOBAL_CONFIG && value == Some(OsStr::new("/dev/null"))
            )
        );
        Ok(wrapped)
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
    /// `cmd\git.exe` beside its `mingw64` folder, all in one temp folder. Output: the folders.
    fn made() -> Made {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let (project, folder) = (root.join("project"), root.join(OWNER_GIT_FOLDER));
        let (git, grants) = (
            root.join("Git").join("cmd").join("git.exe"),
            root.join(GRANTS_FOLDER),
        );
        let platform = root.join("Git").join("mingw64");
        for made in [&project, git.parent().unwrap(), &grants, &platform] {
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

    /// Lays out a stand-in Git for Windows under `root`: each relative file is written empty
    /// and each relative folder made. Inputs: the root, the files and the folders. Output:
    /// the root's `cmd\git.exe` or, with no such file, the first file.
    fn laid_out(root: &Path, files: &[&str], folders: &[&str]) -> PathBuf {
        for folder in folders {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
        }
        let cmd = root.join("cmd").join("git.exe");
        if cmd.is_file() {
            cmd
        } else {
            root.join(files[0])
        }
    }

    /// Asserts owner git refuses to install for `git` with `words` in its reason and writes no
    /// profile. Inputs: the folders, the owner-git folder, the project, git and the words.
    fn refused_install(made: &Made, folder: &Path, project: &Path, git: &Path, words: &str) {
        let helper = std::env::current_exe().unwrap();
        match OwnerGit::install(folder, project, git, &helper, &made.grants) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(reason.contains(words), "{}: {reason}", git.display())
            }
            other => panic!(
                "{}: expected a refusal naming {words}, got {other:?}",
                git.display()
            ),
        }
        let folder = canonical(folder).unwrap();
        assert!(
            !places(&folder, &canonical(project).unwrap())
                .profile
                .exists()
        );
    }

    #[test]
    fn owner_git_reads_only_a_folder_that_holds_git_for_windows_markers() {
        let made = made();
        for (name, files, folders) in [
            ("UserBin", &["bin/git.exe"][..], &[][..]),
            ("UserCmd", &["cmd/git.exe"][..], &[][..]),
            ("NoCmd", &["bin/git.exe"][..], &["mingw64"][..]),
            (
                "NoPlatform",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["usr/bin"][..],
            ),
        ] {
            let git = laid_out(&made.root.join(name), files, folders);
            refused_install(&made, &made.folder, &made.project, &git, "Git for Windows");
        }
        for (name, platform) in [
            ("Full", "mingw64"),
            ("Arm", "clangarm64"),
            ("Old", "mingw32"),
        ] {
            let git = laid_out(
                &made.root.join(name),
                &["cmd/git.exe", "bin/git.exe"],
                &[platform],
            );
            let project = made.root.join(format!("project-{name}"));
            fs::create_dir_all(&project).unwrap();
            let helper = std::env::current_exe().unwrap();
            let owner = OwnerGit::install(&made.folder, &project, &git, &helper, &made.grants)
                .unwrap_or_else(|problem| panic!("{name}: {problem:?}"));
            assert_eq!(
                BoundaryProfile::load(owner.profile()).unwrap().read_roots,
                [made.root.join(name).to_str().unwrap().to_owned()],
                "{name}"
            );
        }
    }

    #[test]
    fn owner_git_refuses_an_install_folder_that_holds_the_project_or_its_own_folder() {
        let made = made();
        let work = made.root.join("work");
        let project = work.join("project");
        fs::create_dir_all(&project).unwrap();
        let git = laid_out(&work, &["cmd/git.exe"], &["mingw64"]);
        refused_install(&made, &made.folder, &project, &git, "holds");
        let engine = made.root.join("engine");
        let folder = engine.join(OWNER_GIT_FOLDER);
        let git = laid_out(&engine, &["cmd/git.exe"], &["mingw64"]);
        refused_install(&made, &folder, &made.project, &git, "holds");
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
