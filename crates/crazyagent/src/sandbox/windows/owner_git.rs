use super::{WindowsSandbox, container_for};
use crate::sandbox::{AGENT_HOME_ENV, ProjectSandbox, Sandbox, canonical, clean_absolute};
use kernel_sandbox::error::{SandboxError, invalid};
use kernel_sandbox::net::SandboxNetwork;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folder in the engine's data root that holds, for each project, owner git's home, its
/// temp folder and its boundary profile.
pub const OWNER_GIT_FOLDER: &str = "owner-git";

const PROGRAM_FOLDERS: [&str; 2] = ["cmd", "bin"];
const PLATFORM_FOLDERS: [&str; 5] = ["ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"];
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
/// a platform folder's `bin` such as `ucrt64\bin` or `mingw64\bin`, so the program the launcher
/// starts and its libraries are readable too. Input: git's absolute program. Output: the
/// folder, or none for a program outside those layouts.
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
/// out: `cmd\git.exe` and a platform's `bin\git.exe` such as `ucrt64\bin\git.exe`, so a
/// `git.exe` in any folder's `cmd` or `bin`, or a bare platform folder, never makes that whole
/// folder readable (rv-sec3 #628 F1). Input: the folder above the program. Output: the answer.
fn marked(root: &Path) -> bool {
    root.join("cmd").join("git.exe").is_file()
        && PLATFORM_FOLDERS
            .iter()
            .any(|platform| root.join(platform).join("bin").join("git.exe").is_file())
}

/// Git's program in the plain drive form the boundary starts, such as the engine's lookup
/// gives as `\\?\C:\...`: kept only when the plain path opens the same file as the program
/// (volume serial and file index), since a plain path drops a trailing dot or space and keeps
/// the length limit. Input: git's program. Output: the plain program, or `Invalid` for another
/// file or a form that is no plain drive path.
fn plain_program(git: &Path) -> Result<PathBuf, SandboxError> {
    let plain = canonical(git)?;
    let opened =
        |path: &Path| fs::File::open(path).and_then(|file| crazyagent_win_sandbox::file_id(&file));
    match (opened(git), opened(&plain)) {
        (Ok(program), Ok(named)) if program == named => {}
        _ => {
            return Err(invalid(
                "git's plain path opens another file than its program",
            ));
        }
    }
    if !clean_absolute(&plain) {
        return Err(invalid("git's program is not a plain drive path"));
    }
    Ok(plain)
}

/// The owner's git for one project, run inside an AppContainer of its own with no capability
/// (owner option 1, V244-WIN-GIT-UNC-SIBLINGS), so a `.git` that points git at a share or a
/// folder outside the project reaches nothing. Every command reuses the profile written once.
#[derive(Debug)]
pub struct OwnerGit {
    boundary: WindowsSandbox,
    git: PathBuf,
    tmp: PathBuf,
}

impl OwnerGit {
    /// Makes the project's places under `folder` and writes its boundary profile there for
    /// `git` in its plain form ([`plain_program`]), bound to `helper` and recording grants in
    /// `grants`. Inputs: the owner-git folder, the project, git's absolute program, the helper
    /// and the grants folder. Output: owner git, or the refusal: a project or git no boundary
    /// can hold, or a missing helper.
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
        let git = plain_program(git)?;
        let policy = policy(&project, &git, &places)?;
        let boundary = WindowsSandbox::install(policy, helper, &places.profile, grants)?;
        assert_eq!(boundary.profile(), places.profile);
        Ok(Self {
            boundary,
            git,
            tmp: places.tmp,
        })
    }

    /// The installed boundary profile every wrapped command names.
    pub fn profile(&self) -> &Path {
        self.boundary.profile()
    }

    /// Owner git's temp folder for the project, which its container writes: where a publish
    /// keeps the files its contained git writes beside the project, such as the private index.
    pub fn tmp(&self) -> &Path {
        &self.tmp
    }

    /// The git program the boundary was installed for, the only one [`OwnerGit::wrap`] runs.
    pub fn git(&self) -> &Path {
        &self.git
    }

    /// Wraps one git command built for the project: the same program, arguments, folder and
    /// variables but `GIT_CONFIG_GLOBAL`, which the boundary sets to the null device for every
    /// process ([`AGENT_HOME_ENV`]); the command leaves it unset or names the null device. Default
    /// stdio: the caller sets it again. Input: the command. Output: the wrapped command, or
    /// `Invalid` for another program, global config (a removal among them) or starting folder.
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
    /// `cmd\git.exe` beside its `mingw64\bin\git.exe`, all in one temp folder. Output: the folders.
    fn made() -> Made {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let (project, folder) = (root.join("project"), root.join(OWNER_GIT_FOLDER));
        let (git, grants) = (
            root.join("Git").join("cmd").join("git.exe"),
            root.join(GRANTS_FOLDER),
        );
        let platform = root.join("Git").join("mingw64").join("bin").join("git.exe");
        for made in [
            &project,
            git.parent().unwrap(),
            &grants,
            platform.parent().unwrap(),
        ] {
            fs::create_dir_all(made).unwrap();
        }
        fs::write(&git, "").unwrap();
        fs::write(&platform, "").unwrap();
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
    fn the_git_the_engine_finds_on_the_path_is_one_owner_git_installs_for() {
        let git = crate::github::git_program(std::env::var_os("PATH").as_deref())
            .expect("git on the runner's PATH");
        let root = install_root(&git);
        assert!(
            root.is_some_and(marked),
            "git {} gives the install folder {root:?}",
            git.display()
        );
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
            r"C:\Programs\Git\ucrt64\bin\git.exe",
            r"C:\Programs\Git\clang64\bin\git.exe",
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
    fn owner_git_installed_from_a_verbatim_program_wraps_its_plain_form() {
        let made = made();
        let verbatim = PathBuf::from(format!(r"\\?\{}", made.git.display()));
        let helper = std::env::current_exe().unwrap();
        let owner = OwnerGit::install(
            &made.folder,
            &made.project,
            &verbatim,
            &helper,
            &made.grants,
        )
        .unwrap();
        assert_eq!(owner.git(), made.git);
        let mut git = Command::new(owner.git());
        git.current_dir(&made.project).arg("--version");
        let wrapped = owner.wrap(&git).unwrap();
        assert_eq!(wrapped.get_args().nth(3), Some(made.git.as_os_str()));
    }

    #[test]
    fn owner_git_refuses_a_verbatim_program_whose_plain_path_opens_another_file() {
        let made = made();
        for tail in [".", " "] {
            let verbatim = PathBuf::from(format!(r"\\?\{}{tail}", made.git.display()));
            fs::write(&verbatim, "another git").unwrap();
            refused_install(
                &made,
                &made.folder,
                &made.project,
                &verbatim,
                "another file",
            );
        }
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

    /// CR-0443 unc9 r2: a share row waits while another holds the runner's sessions, so no
    /// reset or session read of one row falls inside another's; the waiting hold sees the
    /// first row's work already finished.
    #[test]
    fn a_share_row_waits_while_another_holds_the_runner_sessions() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let inside = Arc::new(AtomicBool::new(true));
        let first = farm::hold();
        let waiting = {
            let inside = inside.clone();
            std::thread::spawn(move || {
                let _second = farm::hold();
                inside.load(Ordering::SeqCst)
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        inside.store(false, Ordering::SeqCst);
        drop(first);
        assert!(
            !waiting.join().unwrap(),
            "the second hold began while the first row was inside"
        );
    }

    #[test]
    fn owner_git_reads_only_a_folder_that_holds_git_for_windows_markers() {
        let made = made();
        for (name, platform) in [
            ("Ucrt", "ucrt64"),
            ("Clang", "clang64"),
            ("Full", "mingw64"),
            ("Arm", "clangarm64"),
            ("Old", "mingw32"),
        ] {
            let platform_git = format!("{platform}/bin/git.exe");
            let git = laid_out(
                &made.root.join(name),
                &["cmd/git.exe", "bin/git.exe", platform_git.as_str()],
                &[],
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
        for (name, files, folders) in [
            ("UserBin", &["bin/git.exe"][..], &[][..]),
            ("UserCmd", &["cmd/git.exe"][..], &[][..]),
            (
                "NoCmd",
                &["bin/git.exe", "mingw64/bin/git.exe"][..],
                &[][..],
            ),
            (
                "NoPlatform",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["usr/bin"][..],
            ),
            (
                "BareMingw",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["mingw64/bin"][..],
            ),
            (
                "BareUcrt",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["ucrt64/bin"][..],
            ),
        ] {
            let git = laid_out(&made.root.join(name), files, folders);
            refused_install(&made, &made.folder, &made.project, &git, "Git for Windows");
        }
    }

    #[test]
    fn owner_git_refuses_an_install_folder_that_holds_the_project_or_its_own_folder() {
        let made = made();
        let work = made.root.join("work");
        let project = work.join("project");
        fs::create_dir_all(&project).unwrap();
        let git = laid_out(&work, &["cmd/git.exe", "mingw64/bin/git.exe"], &[]);
        refused_install(&made, &made.folder, &project, &git, "holds");
        let engine = made.root.join("engine");
        let folder = engine.join(OWNER_GIT_FOLDER);
        let git = laid_out(&engine, &["cmd/git.exe", "mingw64/bin/git.exe"], &[]);
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

/// What the share-stop rows share on the test farm: the shares and the runner user that
/// `.github/farm/unc-share-setup.ps1` sets up, the server session that is the measure (P1),
/// and the planted shapes that point git at those shares (V244-WIN-GIT-UNC-SIBLINGS N1).
#[cfg(test)]
pub(crate) mod farm {
    use super::{OWNER_GIT_FOLDER, OwnerGit};
    use crate::sandbox::windows::{GRANTS_FOLDER, HELPER_NAME};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    /// The lock every share row holds from its session reset to its last session read.
    static SESSIONS: Mutex<()> = Mutex::new(());

    /// Proof a row holds the runner's SMB sessions: the server's session table is machine-wide,
    /// so two share rows in one test process would erase or add each other's sessions
    /// (CodeRabbit CR-0443 unc9 r2). Dropping it lets the next row in.
    pub(crate) struct Sessions(
        #[allow(dead_code, reason = "held only for its drop")] MutexGuard<'static, ()>,
    );

    /// Waits until no other row holds the sessions, then holds them. A row that failed while
    /// holding them leaves nothing to undo, so a poisoned lock is taken as it is. Output: the
    /// hold, kept for the whole row.
    pub(crate) fn hold() -> Sessions {
        Sessions(SESSIONS.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Each planted shape with the share it names, as the setup script opens them.
    pub(crate) const SHAPES: [(&str, &str); 5] = [
        ("alternates", "unc-alternates"),
        ("nested", "unc-nested"),
        ("include", "unc-include"),
        ("commondir", "unc-commondir"),
        ("exclude-link", "unc-exclude"),
    ];

    /// A value the setup script exported for the rows. Input: the variable name. Output:
    /// its value; a missing one fails the row, which runs only after that script.
    fn exported(name: &str) -> String {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} is set by unc-share-setup.ps1 on the test farm"))
    }

    /// The path `rest` inside the share `name` at the runner's own IPv4 address. Inputs: the
    /// share and the path inside it. Output: the UNC path, with backslashes.
    fn share(name: &str, rest: &str) -> String {
        format!(
            r"\\{}\{name}\{rest}",
            exported("CRAZYAGENT_FARM_SHARE_HOST")
        )
    }

    /// Runs `net` with `args` and returns what it printed. Input: the arguments. Output:
    /// standard output and error, as one text.
    fn net(args: &[&str]) -> String {
        let output = kernel_processes::spawn_gate::collected(
            Command::new("net").args(args).stdin(Stdio::null()),
        )
        .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// Ends every server session, so the next read sees only what a row's git opened.
    /// Input: the row's hold on the sessions.
    pub(crate) fn reset_sessions(held: &Sessions) {
        net(&["session", "/delete", "/y"]);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!server_session(held), "a server session outlived the reset");
    }

    /// True when the runner's SMB server holds a session, the effect a share open leaves
    /// (P1: no container open made one, every owner open did). Input: the row's hold on the
    /// sessions. Output: the answer.
    pub(crate) fn server_session(_held: &Sessions) -> bool {
        !net(&["session"]).contains("There are no entries")
    }

    /// Makes the runner user own `folder` and everything in it, as an owner's own project
    /// is: the elevated runner's files are owned by Administrators, which contained git
    /// refuses as dubious ownership (P2e). Input: the folder.
    pub(crate) fn own(folder: &Path) {
        let sid = exported("CRAZYAGENT_FARM_USER_SID");
        let output = kernel_processes::spawn_gate::collected(
            Command::new("icacls")
                .arg(folder)
                .args(["/setowner", &format!("*{sid}"), "/T", "/C", "/L", "/Q"])
                .stdin(Stdio::null()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "icacls /setowner: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    /// Installs owner git for `project` with the helper beside the test binary's folder and
    /// owner git's and the grants' folders in `place`, making the grants folder as the engine's
    /// data root holds it, then runs one wrapped `git --version` to exit 0, as a prepare would
    /// before any timed step. Inputs: the temp folder, the project and git's program. Output:
    /// owner git; a refusal, or a contained git that fails, fails the row with its stderr.
    pub(crate) fn installed(place: &Path, project: &Path, git: &Path) -> OwnerGit {
        let tests = std::env::current_exe().unwrap();
        let helper: PathBuf = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert!(
            helper.is_file(),
            "build crazyagent-win-boundary first: {}",
            helper.display()
        );
        let grants = place.join(GRANTS_FOLDER);
        std::fs::create_dir_all(&grants).unwrap();
        let owner = OwnerGit::install(
            &place.join(OWNER_GIT_FOLDER),
            project,
            git,
            &helper,
            &grants,
        )
        .unwrap_or_else(|refused| {
            panic!(
                "owner git install for {} with {}: {refused:?}",
                project.display(),
                git.display()
            )
        });
        let mut version = Command::new(owner.git());
        version
            .arg("--version")
            .current_dir(crate::sandbox::canonical(project).unwrap())
            .env("GIT_CONFIG_GLOBAL", crate::github::NULL_DEVICE);
        let mut wrapped = owner.wrap(&version).unwrap_or_else(|refused| {
            panic!(
                "owner git wrap for {} with {}: {refused:?}",
                project.display(),
                owner.git().display()
            )
        });
        let ran = kernel_processes::spawn_gate::collected(&mut wrapped).unwrap();
        assert!(
            ran.status.success(),
            "owner git --version in its container: {:?}, stderr: {}",
            ran.status,
            String::from_utf8_lossy(&ran.stderr)
        );
        owner
    }

    /// Plants `shape` in the repository whose work tree is `project`, pointing git at its
    /// share: an alternates file, a nested gitfile, a config include, a `commondir` or an
    /// `info\exclude` link. Inputs: the project and the shape, one of [`SHAPES`].
    pub(crate) fn plant(project: &Path, shape: &str) {
        let git_dir = project.join(".git");
        let (_, name) = SHAPES
            .iter()
            .find(|(known, _)| *known == shape)
            .unwrap_or_else(|| panic!("unknown shape {shape}"));
        match shape {
            "alternates" => std::fs::write(
                git_dir.join("objects").join("info").join("alternates"),
                format!("{}\n", share(name, "objects")),
            )
            .unwrap(),
            "nested" => {
                let sub = project.join("sub");
                std::fs::create_dir_all(&sub).unwrap();
                std::fs::write(sub.join("x.txt"), "x").unwrap();
                std::fs::write(
                    sub.join(".git"),
                    format!("gitdir: {}\n", share(name, "nested")),
                )
                .unwrap();
            }
            "include" => {
                let config = git_dir.join("config");
                let kept = std::fs::read_to_string(&config).unwrap();
                let path = share(name, "cfg").replace('\\', "/");
                std::fs::write(&config, format!("{kept}[include]\n\tpath = {path}\n")).unwrap();
            }
            "commondir" => std::fs::write(
                git_dir.join("commondir"),
                format!("{}\n", share(name, "common")),
            )
            .unwrap(),
            "exclude-link" => {
                let exclude = git_dir.join("info").join("exclude");
                std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
                let _ = std::fs::remove_file(&exclude);
                std::os::windows::fs::symlink_file(share(name, "exclude"), &exclude).unwrap();
            }
            _ => unreachable!(),
        }
    }
}
