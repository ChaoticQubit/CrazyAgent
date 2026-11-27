use super::profile::WindowsSandbox;
use crate::error::{SandboxError, invalid};
use crate::net::SandboxNetwork;
use crate::paths::{canonical, clean_absolute};
use crate::profile::ProfileHooks;
use crate::profile::ProjectSandbox;
use crate::profile::command::Sandbox;
use crate::profile::consts::AGENT_HOME_ENV;
use crate::windows::container_for;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folder in the engine's data root that holds, for each project, owner git's home, its
/// temp folder and its boundary profile.
pub const OWNER_GIT_FOLDER: &str = "owner-git";

const PROGRAM_FOLDERS: [&str; 2] = ["cmd", "bin"];
const PLATFORM_FOLDERS: [&str; 5] = ["ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"];
pub const GLOBAL_CONFIG: &str = "GIT_CONFIG_GLOBAL";

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
/// and temp folder writable, since publish's `git init` makes `.git` there, the engine root's
/// secrets guarded, and Git for Windows' install folder readable once it holds the markers
/// ([`marked`]) and neither is nor holds the project or owner git's folder (the owner home is
/// `allow_read`'s check). Inputs: the hooks, the engine root, the canonical project, git's
/// program and the places. Output: the policy, or why not.
pub fn policy(
    hooks: &'static ProfileHooks,
    engine: &Path,
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
        hooks,
        project,
        &places.home,
        &places.tmp,
        SandboxNetwork::Only(Vec::new()),
    )?
    .as_job_folder()
    .guard_engine(engine)?
    .allow_read(&root)
}

/// True when a folder holds Git for Windows' markers, as every installer and MinGit lay them out:
/// `cmd\git.exe` and a platform's `bin\git.exe` such as `ucrt64\bin\git.exe`, so a `git.exe` in any
/// folder's `cmd` or `bin`, or a bare platform folder, never makes that whole folder readable.
/// Input: the folder above the program. Output: the answer.
pub fn marked(root: &Path) -> bool {
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

/// The owner's git for one project, run inside an AppContainer of its own with no capability (owner
/// option 1), so a `.git` that points git at a share or a folder outside the project reaches
/// nothing. Every command reuses the profile written once.
#[derive(Debug)]
pub struct OwnerGit {
    boundary: WindowsSandbox,
    git: PathBuf,
    tmp: PathBuf,
}

impl OwnerGit {
    /// Makes the project's places under `folder` and writes its boundary profile there for
    /// `git` in its plain form ([`plain_program`]), bound to `helper` and recording grants in
    /// `grants`. Inputs: the hooks, the engine root, the owner-git folder, the project, git's
    /// absolute program, the helper and the grants folder. Output: owner git, or the refusal:
    /// a project or git no boundary can hold, or a missing helper.
    pub fn install(
        hooks: &'static ProfileHooks,
        engine: &Path,
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
        let policy = policy(hooks, engine, &project, &git, &places)?;
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
                if value != Some(OsStr::new(kernel_common::names::NULL_DEVICE)) {
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
