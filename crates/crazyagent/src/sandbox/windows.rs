use super::{
    MODEL_LOOPBACK_WINDOWS_LINE, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork,
    SandboxRemote, agent_env, canonical, clean_absolute, invalid,
};
use crazyagent_win_sandbox::plan::plan;
use crazyagent_win_sandbox::profile::{BoundaryProfile, Capability, Guard, Guarded, check_path};
use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The owner's own git for one project inside an AppContainer of its own, with no capability,
/// so a `.git` that points git elsewhere reaches nothing (V244-WIN-GIT-UNC-SIBLINGS).
pub mod owner_git;

/// The boundary helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-win-boundary.exe";

/// Why a Windows launch is refused when the boundary helper is missing, in plain words.
pub const APPCONTAINER_MISSING: &str = "Windows agents start through crazyagent-win-boundary.exe next to the engine, which is missing, so no agent starts unconfined";

/// Why a Seatbelt profile is refused on Windows, in plain words.
pub const SEATBELT_MISSING: &str =
    "Windows has no Seatbelt; agents start through the AppContainer boundary";

/// The most processes one agent may run at once inside its job.
pub const ACTIVE_PROCESSES: u32 = 512;

/// Why an agent's profile is refused when one of its writable roots reaches the project's git
/// folder, in plain words (rv-sec3 #764 L1).
pub const WRITES_OWNER_GIT: &str = "an agent profile can't write the project's git folder";

/// Who a boundary profile is for: an agent, which never writes the project's git folder, or the
/// owner's own git ([`owner_git`]), the one profile that writes it, since the engine then runs
/// git there as itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    Agent,
    OwnerGit,
}
/// The lock file git writes beside `.git\config` while it changes the config. The Windows
/// boundary leaves it out of the protected paths: Windows cannot refuse creating one name, so
/// guarding a lock that is normally missing would keep `.git` from taking new entries, git's
/// own `index.lock` among them. A lock an agent makes never becomes the config: `config` is
/// read only, so no agent replaces or deletes it (DEC-193 D6, step 0c).
const GIT_CONFIG_LOCK: &str = "config.lock";

/// The folder in the engine's data root where the boundary helper records the access entries
/// it grants each agent's container, so a later, narrower profile can take them back.
pub const GRANTS_FOLDER: &str = "boundary-grants";

const CONTAINER_PREFIX: &str = "crazyagent.";
const CONTAINER_HASH_BYTES: usize = 16;
const PROFILE_NAME_MAX: usize = 128;
const SYSTEM_ROOT_DEFAULT: &str = r"C:\Windows";
const PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

const PINNED_VARIABLES: &[&str] = &[
    "USERPROFILE",
    "HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "TMPDIR",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
];

/// The Windows boundary for one project agent. The helper runs the agent in an AppContainer
/// of its own with read and execute on its read roots, modify on its writable roots, read and
/// execute only on the protected paths and nothing on secret paths inside a granted root,
/// inside a job that kills the agent's whole tree when the helper ends (DEC-193).
#[derive(Debug)]
pub struct WindowsSandbox {
    sandbox: ProjectSandbox,
    helper: PathBuf,
    profile: PathBuf,
}

impl WindowsSandbox {
    /// Makes each missing guarded folder ([`make_guarded_folders`]), then writes the boundary
    /// profile for `sandbox` at `profile`, a `.sb` file replaced atomically, bound to `helper`;
    /// the profile and the helper's `grants` folder lie outside every writable root, so no
    /// agent rewrites what its next launch uses. Inputs: the policy, the helper, the profile
    /// path and the grants folder. Output: the sandbox, or the [`SandboxError`] saying why not.
    pub fn install(
        sandbox: ProjectSandbox,
        helper: &Path,
        profile: &Path,
        grants: &Path,
    ) -> Result<Self, SandboxError> {
        Self::install_for(sandbox, helper, profile, grants, Writer::Agent)
    }

    /// [`WindowsSandbox::install`] for one writer: an agent's profile is refused before any
    /// folder is made when it would write the project's git folder ([`writes_owner_git`]).
    /// Inputs: the policy, the helper, the profile path, the grants folder and the writer.
    /// Output: the sandbox, or the [`SandboxError`] saying why not.
    fn install_for(
        sandbox: ProjectSandbox,
        helper: &Path,
        profile: &Path,
        grants: &Path,
        writer: Writer,
    ) -> Result<Self, SandboxError> {
        let grants = Some(grants)
            .filter(|grants| clean_absolute(grants) && grants.is_dir())
            .ok_or_else(|| invalid("grants folder must exist"))?;
        if writer == Writer::Agent && writes_owner_git(&sandbox) {
            return Err(invalid(WRITES_OWNER_GIT));
        }
        make_guarded_folders(&sandbox)?;
        let boundary = profile_for(&sandbox, &canonical(grants)?, writer)?;
        if let Some(root) = crazyagent_win_sandbox::unreadable_read_root(&boundary) {
            eprintln!("sandbox_read_root_refused cause=unreadable folder={root}");
            return Err(SandboxError::Unreadable(PathBuf::from(root)));
        }
        let helper = existing_helper(helper)?;
        let parent = profile
            .parent()
            .filter(|parent| clean_absolute(parent) && parent.is_dir())
            .ok_or_else(|| invalid("profile directory must exist"))?;
        let parent = canonical(parent)?;
        let name = profile
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.ends_with(".sb") && name.len() <= PROFILE_NAME_MAX)
            .ok_or_else(|| invalid("profile name must end in .sb"))?;
        let target = parent.join(name);
        if !clean_absolute(&target)
            || sandbox
                .writable_roots()
                .any(|root| target.starts_with(root))
        {
            return Err(invalid(
                "profile must live outside the sandbox's writable roots",
            ));
        }
        let text = boundary
            .encode()
            .map_err(|error| invalid(&error.to_string()))?;
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&staging, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        assert!(target.is_file());
        Ok(Self {
            sandbox,
            helper,
            profile: target,
        })
    }

    /// The program every wrapped command starts: the boundary helper.
    pub fn launcher(&self) -> &Path {
        &self.helper
    }

    /// The project policy this sandbox holds.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed boundary profile the helper reads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    fn base_env(&self) -> Vec<(&'static str, OsString)> {
        let home = self.sandbox.agent_home();
        let tmp = self.sandbox.agent_tmp().as_os_str();
        let system = system_root();
        let drive = system.chars().take(2).collect::<String>();
        let path = format!(
            r"{system}\System32;{system};{system}\System32\Wbem;{system}\System32\WindowsPowerShell\v1.0"
        );
        vec![
            ("USERPROFILE", home.as_os_str().to_owned()),
            ("HOME", home.as_os_str().to_owned()),
            ("APPDATA", home.join("AppData").join("Roaming").into()),
            ("LOCALAPPDATA", home.join("AppData").join("Local").into()),
            ("TEMP", tmp.to_owned()),
            ("TMP", tmp.to_owned()),
            ("TMPDIR", tmp.to_owned()),
            ("SystemRoot", system.clone().into()),
            ("SystemDrive", drive.into()),
            ("windir", system.clone().into()),
            ("ComSpec", format!(r"{system}\System32\cmd.exe").into()),
            ("PATHEXT", PATHEXT.into()),
            ("PATH", path.into()),
        ]
    }
}

impl Sandbox for WindowsSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness
            .get_current_dir()
            .unwrap_or(&self.sandbox.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.sandbox.project_root)
                .chain(self.sandbox.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        let owned = self.base_env();
        let base: Vec<(&str, &OsStr)> = owned
            .iter()
            .map(|(name, value)| (*name, value.as_os_str()))
            .collect();
        let pinned = |name: &OsStr| {
            PINNED_VARIABLES
                .iter()
                .any(|pinned| name.eq_ignore_ascii_case(pinned))
        };
        let env = agent_env(&base, harness.get_envs().filter(|&(name, _)| !pinned(name)))?;
        let mut command = boundary_command(&self.helper, &self.profile, program, &env);
        command.args(harness.get_args()).current_dir(cwd);
        assert_eq!(command.get_program(), self.helper.as_os_str());
        assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
        Ok(command)
    }
}

/// Builds a Windows agent command without a bound sandbox: the helper next to the engine's
/// executable with `--profile <profile> -- <program>` and exactly [`agent_env`]. Inputs: an
/// installed boundary profile, the absolute program and both environments. Output: the
/// command, `Unavailable` without the helper, or `Invalid` for an unsafe input.
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let helper = existing_helper(&helper_beside_engine()?)?;
    if !clean_absolute(profile) || !profile.is_file() {
        return Err(invalid("boundary profile must be an existing plain file"));
    }
    if !clean_absolute(program) {
        return Err(invalid("harness program must be an absolute plain path"));
    }
    let env = agent_env(base, harness)?;
    let command = boundary_command(&helper, profile, program, &env);
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// The AppContainer capabilities for a network reach (DESIGN-0256 O2a): the internet client
/// when open or limited to ports on any host, a limit the AppContainer cannot hold and so
/// widens; none when no endpoint is allowed; and [`MODEL_LOOPBACK_WINDOWS_LINE`] for any
/// loopback port, which it cannot reach (P6). Input: the network. Output: the list or refusal.
pub fn capabilities(network: &SandboxNetwork) -> Result<Vec<Capability>, SandboxError> {
    match network {
        SandboxNetwork::Open => Ok(vec![Capability::InternetClient]),
        SandboxNetwork::Only(remotes) if remotes.is_empty() => Ok(Vec::new()),
        SandboxNetwork::Only(remotes)
            if remotes
                .iter()
                .any(|remote| matches!(remote, SandboxRemote::Loopback(_))) =>
        {
            Err(SandboxError::Unavailable(
                MODEL_LOOPBACK_WINDOWS_LINE.into(),
            ))
        }
        SandboxNetwork::Only(_) => Ok(vec![Capability::InternetClient]),
    }
}

/// The AppContainer name for one agent of one project: `crazyagent.` and 32 hex digits of
/// the SHA-256 of the project folder and the agent's home, so each agent home, which may
/// hold account sign-ins, is reachable by its own container only. Input: the policy.
/// Output: a name the helper's container check accepts.
pub fn container_name(sandbox: &ProjectSandbox) -> String {
    container_for(sandbox.project_root(), sandbox.agent_home())
}

/// The AppContainer name [`container_name`] gives a policy with this project folder and agent
/// home, for a release whose folders are already gone (V243-WIN-CHECK-PROFILE-BIND).
/// Inputs: the project folder and the agent's home. Output: the container name.
pub(crate) fn container_for(project_root: &Path, agent_home: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_root.as_os_str().as_encoded_bytes());
    hasher.update(b"\n");
    hasher.update(agent_home.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let name: String = std::iter::once(CONTAINER_PREFIX.to_owned())
        .chain(
            digest[..CONTAINER_HASH_BYTES]
                .iter()
                .map(|byte| format!("{byte:02x}")),
        )
        .collect();
    assert_eq!(
        name.len(),
        CONTAINER_PREFIX.len() + 2 * CONTAINER_HASH_BYTES
    );
    name
}

/// The boundary profile for a policy: container, network capabilities, read and writable
/// roots, the protected paths kept read-only but git's config lock, and the project's root
/// `context` while `.crazyagent` is a real folder, since the state folder then holds the
/// context the engine reads ([`crate::projects_home::state_folder`]) and a missing root
/// `context` would keep the project root from taking new entries; none for a job folder
/// ([`ProjectSandbox::as_job_folder`]); plain drive secret paths hidden, and the grants folder.
/// The helper's plan check runs here too, so a policy it would refuse, such as a grants folder
/// in a writable root, is refused at install. Inputs: the policy and the grants folder.
/// Output: the profile, or the refusal.
pub fn boundary_profile(
    sandbox: &ProjectSandbox,
    grants: &Path,
) -> Result<BoundaryProfile, SandboxError> {
    profile_for(sandbox, grants, Writer::Agent)
}

/// Tells whether an agent under `sandbox` could write the project's git folder, where the
/// engine's own git reads hooks, refs and objects as the engine: a writable root inside any
/// `.git`, or one that holds or lies in the `.git` of the project root, a read root or a write
/// root, when that is a folder or a link rather than the plain file a worktree has. No guard
/// holds a whole git folder (`commondir` and refs stay writable), so such a root is never
/// allowed. Input: the policy. Output: the answer.
fn writes_owner_git(sandbox: &ProjectSandbox) -> bool {
    let in_git = |root: &Path| {
        root.components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
    };
    let gits: Vec<PathBuf> = std::iter::once(sandbox.project_root())
        .chain(sandbox.read_roots().iter().map(PathBuf::as_path))
        .chain(sandbox.writable_roots().map(PathBuf::as_path))
        .map(|root| root.join(".git"))
        .filter(|git| fs::symlink_metadata(git).is_ok_and(|meta| !meta.is_file()))
        .collect();
    sandbox.writable_roots().any(|root| {
        in_git(root)
            || gits
                .iter()
                .any(|git| root.starts_with(git) || git.starts_with(root))
    })
}

/// [`boundary_profile`] for one writer: an agent's profile that would write the project's git
/// folder ([`writes_owner_git`]) is refused; owner git's is not. Inputs: the policy, the grants
/// folder and the writer. Output: the profile, or the refusal.
fn profile_for(
    sandbox: &ProjectSandbox,
    grants: &Path,
    writer: Writer,
) -> Result<BoundaryProfile, SandboxError> {
    if writer == Writer::Agent && writes_owner_git(sandbox) {
        return Err(invalid(WRITES_OWNER_GIT));
    }
    let capabilities = capabilities(sandbox.network())?;
    let read_roots = sandbox
        .read_roots()
        .iter()
        .map(|root| drive_path(root))
        .collect::<Result<Vec<_>, _>>()?;
    let write_roots = sandbox
        .writable_roots()
        .map(|root| drive_path(root))
        .collect::<Result<Vec<_>, _>>()?;
    let root = sandbox.project_root();
    let config_lock = root.join(".git").join(GIT_CONFIG_LOCK);
    let unread = (crate::projects_home::state_folder(root) != root).then(|| root.join("context"));
    let guards = sandbox.holds_project_guards();
    assert!(guards || sandbox.confinements() == 0);
    let protected = if guards {
        sandbox.protected_paths()
    } else {
        Vec::new()
    };
    let mut guarded = protected
        .iter()
        .filter(|path| **path != config_lock && Some(*path) != unread.as_ref())
        .map(|path| {
            Ok(Guarded {
                path: drive_path(path)?,
                guard: Guard::ReadOnly,
            })
        })
        .collect::<Result<Vec<_>, SandboxError>>()?;
    guarded.extend(sandbox.secret_paths().iter().filter_map(|path| {
        drive_path(path).ok().map(|path| Guarded {
            path,
            guard: Guard::Hidden,
        })
    }));
    let profile = BoundaryProfile {
        container: container_name(sandbox),
        capabilities,
        read_roots,
        write_roots,
        guarded,
        active_processes: ACTIVE_PROCESSES,
        grants: drive_path(grants)?,
    };
    profile
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    plan(&profile, |_| true).map_err(invalid)?;
    Ok(profile)
}

/// Makes `.git\hooks`, `.git\modules` and `.crazyagent\context` when missing, empty, in a parent
/// that is a real folder, never replacing an entry and never making `.git` or `.crazyagent`,
/// which would move a legacy project's state folder (DEC-193 F2): the helper keeps the entries
/// of the folder above a missing guarded path, which would stop git's `index.lock`. A job
/// folder holds no guards, so none is made there. Input: the policy. Output: how many were
/// made, or the failure.
fn make_guarded_folders(sandbox: &ProjectSandbox) -> Result<usize, SandboxError> {
    if !sandbox.holds_project_guards() {
        return Ok(0);
    }
    let root = sandbox.project_root();
    let git = root.join(".git");
    let state = root.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    let wanted = [
        git.join("hooks"),
        git.join("modules"),
        state.join("context"),
    ];
    let guarded = sandbox.protected_paths();
    assert!(wanted.iter().all(|folder| guarded.contains(folder)));
    let mut made = 0;
    for folder in &wanted {
        let parent = folder
            .parent()
            .expect("a guarded folder sits in the project");
        let real = fs::symlink_metadata(parent)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
        if !real {
            continue;
        }
        match fs::create_dir(folder) {
            Ok(()) => made += 1,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    assert!(made <= wanted.len());
    Ok(made)
}

fn drive_path(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .filter(|text| check_path(text).is_ok())
        .map(str::to_owned)
        .ok_or_else(|| invalid("a sandbox path is not a plain drive path"))
}

/// The engine's grants folder, [`GRANTS_FOLDER`] in its data root, made when missing. Output:
/// the folder, `Unavailable` when the data root is unknown, or the I/O error.
pub(super) fn grants_folder() -> Result<PathBuf, SandboxError> {
    let layout =
        crate::paths::Layout::current().map_err(|error| SandboxError::Unavailable(error.0))?;
    let grants = layout.root().join(GRANTS_FOLDER);
    fs::create_dir_all(&grants)?;
    assert!(grants.is_dir());
    Ok(grants)
}

/// Where the boundary helper ships: beside the engine's own executable. Output: the path, or
/// the error reading the executable's path.
pub fn helper_beside_engine() -> std::io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name(HELPER_NAME))
}

/// The boundary helper at `helper`, when it is an existing absolute plain file. Output: the
/// path, `Unavailable` with [`APPCONTAINER_MISSING`] when it is missing, or `Invalid`.
pub(super) fn existing_helper(helper: &Path) -> Result<PathBuf, SandboxError> {
    if !helper.is_file() {
        return Err(SandboxError::Unavailable(APPCONTAINER_MISSING.into()));
    }
    if !clean_absolute(helper) {
        return Err(invalid(
            "the boundary helper must be an absolute plain file",
        ));
    }
    Ok(helper.to_path_buf())
}

fn boundary_command(
    helper: &Path,
    profile: &Path,
    program: &Path,
    env: &[(OsString, OsString)],
) -> Command {
    let mut command = Command::new(helper);
    command
        .arg(PROFILE_FLAG)
        .arg(profile)
        .arg(SEPARATOR)
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)));
    command
}

fn system_root() -> String {
    std::env::var("SystemRoot")
        .ok()
        .filter(|root| check_path(root).is_ok())
        .unwrap_or_else(|| SYSTEM_ROOT_DEFAULT.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{InstalledSandbox, LinuxStrict, LinuxSupervisor, SandboxRemote};
    use crazyagent_win_sandbox::profile::check_container;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Folders {
        _temp: tempfile::TempDir,
        project: PathBuf,
        home: PathBuf,
        tmp: PathBuf,
        profiles: PathBuf,
        grants: PathBuf,
    }

    fn folders() -> Folders {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let project = root.join("project");
        let home = root.join("agent").join("home");
        let tmp = root.join("agent").join("tmp");
        let profiles = root.join("profiles");
        let grants = root.join("grants");
        for folder in [
            &project,
            &project.join("context"),
            &home,
            &tmp,
            &profiles,
            &grants,
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(project.join("context").join("brief.md"), "brief\r\n").unwrap();
        Folders {
            _temp: temp,
            project,
            home,
            tmp,
            profiles,
            grants,
        }
    }

    fn policy(folders: &Folders, network: SandboxNetwork) -> ProjectSandbox {
        ProjectSandbox::new(&folders.project, &folders.home, &folders.tmp, network).unwrap()
    }

    fn cmd() -> PathBuf {
        PathBuf::from(system_root())
            .join("System32")
            .join("cmd.exe")
    }

    #[test]
    fn a_host_or_port_limit_widens_to_the_internet_client_and_a_loopback_port_is_refused() {
        let client = vec![Capability::InternetClient];
        assert_eq!(capabilities(&SandboxNetwork::Open).unwrap(), client);
        assert!(
            capabilities(&SandboxNetwork::Only(Vec::new()))
                .unwrap()
                .is_empty()
        );
        for hosts in [
            vec![SandboxRemote::AnyHost(443)],
            vec![SandboxRemote::AnyHost(443), SandboxRemote::AnyHost(8443)],
        ] {
            assert_eq!(
                capabilities(&SandboxNetwork::Only(hosts)).unwrap(),
                client,
                "a host or port limit is widened to the internet client on Windows (O2a)"
            );
        }
        for remotes in [
            vec![SandboxRemote::Loopback(11_434)],
            vec![SandboxRemote::AnyHost(443), SandboxRemote::Loopback(41_300)],
        ] {
            let refusal = capabilities(&SandboxNetwork::Only(remotes)).unwrap_err();
            assert!(
                crate::sandbox::model_loopback_windows(&refusal),
                "expected the loopback refusal, got {refusal:?}"
            );
        }
        let model_only = crate::sandbox::policy::AgentNetwork::None
            .reach("windows", true, SandboxRemote::AnyHost(443), None)
            .unwrap();
        assert_eq!(capabilities(&model_only).unwrap(), client);
    }

    #[test]
    fn the_profile_keeps_the_policy_and_each_agent_gets_its_own_container() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Open);
        let profile = boundary_profile(&sandbox, &folders.grants).unwrap();
        assert_eq!(profile.capabilities, vec![Capability::InternetClient]);
        assert_eq!(profile.grants, folders.grants.to_str().unwrap());
        assert_eq!(
            profile.write_roots,
            [&folders.project, &folders.home, &folders.tmp]
                .map(|path| path.to_str().unwrap().to_owned())
        );
        let config_lock = sandbox.project_root().join(".git").join(GIT_CONFIG_LOCK);
        for protected in sandbox.protected_paths() {
            let guarded = Guarded {
                path: protected.to_str().unwrap().to_owned(),
                guard: Guard::ReadOnly,
            };
            assert_eq!(profile.guarded.contains(&guarded), protected != config_lock);
        }
        assert!(
            profile
                .guarded
                .iter()
                .any(|item| item.guard == Guard::Hidden && item.path.ends_with(r"\.ssh"))
        );
        assert_eq!(check_container(&profile.container), Ok(()));
        assert!(profile.container.starts_with(CONTAINER_PREFIX));
        let other_home = folders.home.with_file_name("other");
        fs::create_dir_all(&other_home).unwrap();
        let other = ProjectSandbox::new(
            &folders.project,
            &other_home,
            &folders.tmp,
            SandboxNetwork::Open,
        )
        .unwrap();
        assert_ne!(container_name(&other), profile.container);
        assert_eq!(container_name(&sandbox), profile.container);
    }

    #[test]
    fn install_writes_the_profile_outside_the_roots_and_refuses_a_missing_helper() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let inside = folders.project.join("agent.sb");
        assert!(matches!(
            WindowsSandbox::install(sandbox.clone(), &helper, &inside, &folders.grants),
            Err(SandboxError::Invalid(_))
        ));
        assert!(!inside.exists());
        let writable = folders.project.join("grants");
        fs::create_dir_all(&writable).unwrap();
        let beside = folders.profiles.join("c.sb");
        match WindowsSandbox::install(sandbox.clone(), &helper, &beside, &writable) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(
                    reason.contains("the grants folder sits in a writable root"),
                    "{reason}"
                )
            }
            other => panic!("expected the grants folder refusal, got {other:?}"),
        }
        assert!(!beside.exists());
        let absent = folders.profiles.join("absent");
        assert!(matches!(
            WindowsSandbox::install(sandbox.clone(), &helper, &beside, &absent),
            Err(SandboxError::Invalid(_))
        ));
        let missing = folders.profiles.join("missing-helper.exe");
        match WindowsSandbox::install(
            sandbox.clone(),
            &missing,
            &folders.profiles.join("a.sb"),
            &folders.grants,
        ) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, APPCONTAINER_MISSING),
            other => panic!("expected the missing helper refusal, got {other:?}"),
        }
        let path = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox.clone(), &helper, &path, &folders.grants).unwrap();
        let written = BoundaryProfile::load(&path).unwrap();
        assert_eq!(
            written,
            boundary_profile(&sandbox, &folders.grants).unwrap()
        );
        assert_eq!(installed.launcher(), helper.as_path());
        let limited = policy(
            &folders,
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        );
        let widened = folders.profiles.join("b.sb");
        let _held = WindowsSandbox::install(limited, &helper, &widened, &folders.grants).unwrap();
        assert_eq!(
            BoundaryProfile::load(&widened).unwrap().capabilities,
            vec![Capability::InternetClient]
        );
        let local = policy(
            &folders,
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(11_434)]),
        );
        match WindowsSandbox::install(
            local,
            &helper,
            &folders.profiles.join("c.sb"),
            &folders.grants,
        ) {
            Err(error) => assert!(
                crate::sandbox::model_loopback_windows(&error),
                "expected the loopback refusal, got {error:?}"
            ),
            Ok(_) => panic!("a loopback port is refused on Windows"),
        }
    }

    /// V244-WIN-NPM: install refuses a read root no app container reads and this standard
    /// user cannot change, made by an administrator and named in
    /// `CRAZYAGENT_TEST_UNCHANGEABLE_ROOT`, by that folder and before any profile is written,
    /// and installs the same policy reading a folder this user owns.
    #[test]
    #[ignore = "needs an administrator-made folder and a standard user (test farm)"]
    fn install_refuses_a_read_root_only_an_administrator_can_open_to_containers() {
        let folders = folders();
        let fixture = std::env::var_os("CRAZYAGENT_TEST_UNCHANGEABLE_ROOT").unwrap();
        let fixture = canonical(Path::new(&fixture)).unwrap();
        let helper = std::env::current_exe().unwrap();
        let path = folders.profiles.join("agent.sb");
        let refused = policy(&folders, SandboxNetwork::Only(Vec::new()))
            .allow_read(&fixture)
            .unwrap();
        match WindowsSandbox::install(refused, &helper, &path, &folders.grants) {
            Err(SandboxError::Unreadable(folder)) => assert_eq!(folder, fixture),
            other => panic!("expected the unreadable root refusal, got {other:?}"),
        }
        assert!(!path.exists());
        let own = folders.profiles.join("own");
        fs::create_dir_all(&own).unwrap();
        let allowed = policy(&folders, SandboxNetwork::Only(Vec::new()))
            .allow_read(&own)
            .unwrap();
        WindowsSandbox::install(allowed, &helper, &path, &folders.grants).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn wrap_starts_the_harness_through_the_helper_with_pinned_folders() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let installed = WindowsSandbox::install(
            sandbox,
            &helper,
            &folders.profiles.join("agent.sb"),
            &folders.grants,
        )
        .unwrap();
        let mut harness = Command::new(cmd());
        harness
            .arg("/C")
            .arg("exit 0")
            .env("userprofile", r"C:\Users\owner")
            .env("Temp", r"C:\Users\owner\AppData\Local\Temp")
            .env("EDITOR", "notepad")
            .current_dir(&folders.project);
        let wrapped = Sandbox::wrap(&installed, &harness).unwrap();
        assert_eq!(wrapped.get_program(), helper.as_os_str());
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        assert_eq!(args[0], PROFILE_FLAG);
        assert_eq!(args[1], installed.profile().as_os_str());
        assert_eq!(args[2], SEPARATOR);
        assert_eq!(args[3], cmd().as_os_str());
        assert_eq!(&args[4..], [OsStr::new("/C"), OsStr::new("exit 0")]);
        let env: Vec<(String, String)> = wrapped
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        let value = |key: &str| {
            env.iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(value("USERPROFILE"), [folders.home.to_str().unwrap()]);
        assert_eq!(value("TEMP"), [folders.tmp.to_str().unwrap()]);
        assert_eq!(value("EDITOR"), ["notepad"]);
        assert_eq!(value("GIT_TERMINAL_PROMPT"), ["0"]);
        let mut outside = Command::new(cmd());
        outside.current_dir(&folders.profiles);
        assert!(Sandbox::wrap(&installed, &outside).is_err());
        let relative = Command::new("cmd.exe");
        assert!(Sandbox::wrap(&installed, &relative).is_err());
        let mut secret = Command::new(cmd());
        secret.env("GH_TOKEN", "placeholder-value");
        assert!(Sandbox::wrap(&installed, &secret).is_err());
    }

    #[test]
    fn an_installed_windows_sandbox_needs_the_helper_beside_the_engine() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let beside = std::env::current_exe().unwrap().with_file_name(HELPER_NAME);
        let profile = folders.profiles.join("agent.sb");
        match InstalledSandbox::install(sandbox, &profile, LinuxSupervisor::On, LinuxStrict::Off) {
            Ok(installed) => {
                assert!(beside.is_file());
                assert_eq!(installed.launcher(), beside.as_path());
            }
            Err(SandboxError::Unavailable(reason)) => {
                assert!(!beside.is_file());
                assert_eq!(reason, APPCONTAINER_MISSING);
            }
            Err(other) => panic!("unexpected refusal {other}"),
        }
    }

    /// V243-WIN-CONFINED-ASSERTS: a harness wrapped with a boundary profile, as the research
    /// runner wraps its agent, starts at the boundary helper beside the engine with the
    /// profile after `--profile`, inside the project. It needs the helper copied beside the
    /// test binary, where the wrap looks, so it never passes without reaching the assert.
    #[test]
    fn a_profile_wrapped_harness_starts_at_the_helper_beside_the_engine() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.json");
        fs::write(&profile, "{}").unwrap();
        let beside = std::env::current_exe().unwrap().with_file_name(HELPER_NAME);
        assert!(
            beside.is_file(),
            "copy crazyagent-win-boundary beside the test binary first: {}",
            beside.display()
        );
        let mut harness = Command::new(cmd());
        harness
            .arg("/C")
            .arg("exit 0")
            .current_dir(&folders.project);
        let command = ProjectSandbox::wrap(&sandbox, &profile, &harness).unwrap();
        assert_eq!(command.get_program(), beside.as_os_str());
        assert_eq!(command.get_args().nth(1), Some(profile.as_os_str()));
        assert_eq!(command.get_current_dir(), Some(folders.project.as_path()));
    }

    /// The boundary program the confined asserts accept on Windows is only the helper beside
    /// the engine: not the shell, not sandbox-exec and not a helper of the same name in
    /// another folder.
    #[test]
    fn only_the_helper_beside_the_engine_is_the_boundary_program() {
        let tests = std::env::current_exe().unwrap();
        let beside = tests.with_file_name(HELPER_NAME);
        let elsewhere = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert_ne!(beside, elsewhere);
        assert!(crate::sandbox::starts_at_boundary(beside.as_os_str()));
        for program in [
            cmd(),
            elsewhere,
            PathBuf::from(crate::sandbox::SANDBOX_EXEC),
        ] {
            assert!(
                !crate::sandbox::starts_at_boundary(program.as_os_str()),
                "{}",
                program.display()
            );
        }
    }

    #[test]
    fn a_wrapped_agent_cannot_write_a_protected_path_and_dies_with_its_helper() {
        let folders = folders();
        let tests = std::env::current_exe().unwrap();
        let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert!(
            helper.is_file(),
            "build crazyagent-win-boundary first: {}",
            helper.display()
        );
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let installed = WindowsSandbox::install(
            sandbox,
            &helper,
            &folders.profiles.join("agent.sb"),
            &folders.grants,
        )
        .unwrap();
        let brief = folders.project.join("context").join("brief.md");
        let mut overwrite = Command::new(cmd());
        overwrite
            .arg("/C")
            .arg(format!("echo changed>{}", brief.display()))
            .current_dir(&folders.project);
        let status = crate::spawn_gate::status(
            Sandbox::wrap(&installed, &overwrite)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(!status.success());
        assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\r\n");
        let mut shell = Command::new(cmd());
        shell.arg("/K").current_dir(&folders.project);
        let mut running = crate::spawn_gate::spawn(
            Sandbox::wrap(&installed, &shell)
                .unwrap()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let mut stdin = running.stdin.take().unwrap();
        let stdout = running.stdout.take().unwrap();
        let (events, received) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                if line.trim() == "ready" {
                    let _ = events.send("ready");
                }
                line.clear();
            }
            let _ = events.send("closed");
        });
        stdin.write_all(b"echo ready\r\n").unwrap();
        stdin.flush().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("ready"));
        running.kill().unwrap();
        running.wait().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("closed"));
        drop(stdin);
    }

    const CALL: &str = "BOUNDARY_FILE_CALL";

    fn helper() -> PathBuf {
        let tests = std::env::current_exe().unwrap();
        let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert!(
            helper.is_file(),
            "build crazyagent-win-boundary first: {}",
            helper.display()
        );
        helper
    }

    fn quiet(installed: &WindowsSandbox, command: &Command) -> bool {
        crate::spawn_gate::status(
            Sandbox::wrap(installed, command)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .success()
    }

    fn shell(installed: &WindowsSandbox, folders: &Folders, line: &str) -> bool {
        let mut command = Command::new(cmd());
        command.arg("/C").arg(line).current_dir(&folders.project);
        quiet(installed, &command)
    }

    fn call(installed: &WindowsSandbox, folders: &Folders, step: &[&Path]) -> std::process::Output {
        assert!(!step.is_empty() && step.len() <= 2);
        let probe = folders.home.join("file-call.exe");
        if !probe.is_file() {
            fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
        }
        let line = |verb: &str| {
            let paths: Vec<&str> = step.iter().map(|path| path.to_str().unwrap()).collect();
            format!("{verb}|{}", paths.join("|"))
        };
        let verb = if step.len() == 2 { "rename" } else { "rmtree" };
        let mut command = Command::new(&probe);
        command
            .args([
                "sandbox::windows::tests::file_call_child",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CALL, line(verb))
            .current_dir(&folders.project);
        let mut wrapped = Sandbox::wrap(installed, &command).unwrap();
        crate::spawn_gate::collected(&mut wrapped).unwrap()
    }

    fn denied(output: &std::process::Output) -> bool {
        output.status.code() == Some(1)
            && String::from_utf8_lossy(&output.stdout).contains("(os error 5)")
    }

    #[test]
    fn file_call_child() {
        let Ok(line) = std::env::var(CALL) else {
            return;
        };
        let parts: Vec<&str> = line.split('|').collect();
        let done = match parts.as_slice() {
            ["rename", from, to] => fs::rename(from, to),
            ["rmtree", path] => fs::remove_dir_all(path),
            _ => panic!("unknown file call {line}"),
        };
        if let Err(error) = done {
            println!("CALL {line} => {error}");
            std::process::exit(1);
        }
    }

    /// DEC-193 F2 with #764 L1: install makes a missing `.crazyagent\context` empty and keeps what
    /// is already there; it makes nothing under a `.git` that does not exist, and a project that
    /// holds a `.git` folder is refused before any folder is made.
    #[test]
    fn install_makes_each_missing_guarded_folder_and_leaves_what_is_there() {
        let folders = folders();
        let helper = std::env::current_exe().unwrap();
        let git = folders.project.join(".git");
        let state = folders.project.join(".crazyagent");
        fs::create_dir_all(&state).unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let first = folders.profiles.join("a.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &first, &folders.grants).unwrap();
        let context = state.join("context");
        assert!(context.is_dir() && fs::read_dir(&context).unwrap().next().is_none());
        assert!(!git.exists(), "no git folder is made");
        fs::write(context.join("brief.md"), "owner\n").unwrap();
        let second = folders.profiles.join("b.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &second, &folders.grants).unwrap();
        assert_eq!(
            fs::read_to_string(context.join("brief.md")).unwrap(),
            "owner\n",
            "an entry already there is never replaced"
        );
        fs::create_dir_all(&git).unwrap();
        let third = folders.profiles.join("c.sb");
        assert!(refused_for_git(WindowsSandbox::install(
            sandbox,
            &helper,
            &third,
            &folders.grants
        )));
        assert!(!third.exists());
        assert!(!git.join("hooks").exists() && !git.join("modules").exists());
    }

    #[test]
    fn a_legacy_project_gets_no_state_folder_and_keeps_its_root_context_guarded() {
        let folders = folders();
        let helper = std::env::current_exe().unwrap();
        let project = &folders.project;
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("a.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &profile, &folders.grants).unwrap();
        assert!(
            !project.join(".git").exists(),
            "a git-less project stays git-less"
        );
        assert!(
            !project.join(".crazyagent").exists(),
            "making .crazyagent would move the project's state folder"
        );
        let guarded = |sandbox: &ProjectSandbox| -> Vec<String> {
            boundary_profile(sandbox, &folders.grants)
                .unwrap()
                .guarded
                .into_iter()
                .map(|item| item.path)
                .collect()
        };
        let root_context = drive_path(&project.join("context")).unwrap();
        assert!(guarded(&sandbox).contains(&root_context));
        fs::create_dir_all(project.join(".crazyagent")).unwrap();
        assert_eq!(
            crate::projects_home::state_folder(project),
            project.join(".crazyagent")
        );
        let moved = guarded(&sandbox);
        assert!(
            !moved.contains(&root_context),
            "the root context is unread once .crazyagent holds the state"
        );
        assert!(moved.contains(&drive_path(&project.join(".crazyagent").join("context")).unwrap()));
    }

    /// DEC-193 on a project with no `.git` folder (#764 L1 refuses one that has it): the agent
    /// cannot make a git folder or the state context, cannot vacate the root context, and
    /// still edits and renames files it already has.
    #[test]
    fn a_wrapped_agent_cannot_vacate_or_create_a_protected_path_but_still_edits_the_project() {
        let folders = folders();
        let helper = helper();
        let project = &folders.project;
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("README.md"), "readme\r\n").unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        assert!(!run(r"mkdir .git\hooks"));
        assert!(!project.join(".git").exists());
        assert!(!run(r"mkdir .crazyagent\context"));
        assert!(!project.join(".crazyagent").exists());
        let context = project.join("context");
        let vacated = call(&installed, &folders, &[&context, &project.join("moved")]);
        assert!(denied(&vacated), "{vacated:?}");
        assert!(context.join("brief.md").is_file());
        assert!(run(r"echo changed>src\work.txt"));
        let src = project.join("src");
        let renamed = call(
            &installed,
            &folders,
            &[&src.join("work.txt"), &src.join("done.txt")],
        );
        assert!(renamed.status.success(), "{renamed:?}");
        assert!(src.join("done.txt").is_file() && !src.join("work.txt").exists());
        assert!(run("echo changed>README.md"));
        assert_eq!(
            fs::read_to_string(project.join("README.md")).unwrap(),
            "changed\r\n"
        );
    }

    /// #764 L1 and #765: a guarded project-root agent on a repository is refused at install, so
    /// it never writes `.git\commondir` (which would point the engine's git at config and hooks
    /// the agent holds) or takes git's `index.lock`; on the same project with no `.git`, the
    /// agent cannot change or remove the state context, and the root context is its to change.
    #[test]
    fn a_wrapped_agent_on_a_repository_is_refused_before_it_can_write_git_commondir() {
        let folders = folders();
        let helper = helper();
        let project = &folders.project;
        let git = project.join(".git");
        let state = project.join(".crazyagent").join("context");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::write(git.join("config"), "[core]\r\n").unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("brief.md"), "brief\r\n").unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.sb");
        match WindowsSandbox::install(sandbox.clone(), &helper, &profile, &folders.grants) {
            Ok(installed) => {
                let wrote = shell(&installed, &folders, r"echo ..\planted>.git\commondir");
                panic!(
                    "an agent profile installed on a repository; .git\\commondir written: {}",
                    wrote && git.join("commondir").is_file()
                );
            }
            Err(error) => assert!(refused_for_git::<()>(Err(error))),
        }
        assert!(!git.join("commondir").exists());
        assert!(!profile.exists());
        assert!(!git.join("index.lock").exists() && !git.join("index").exists());
        assert!(!git.join("modules").exists());
        fs::remove_dir_all(&git).unwrap();
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        assert!(!run(r"echo planted>.crazyagent\context\brief.md"));
        let removed = call(&installed, &folders, &[&state]);
        assert!(denied(&removed), "{removed:?}");
        assert_eq!(
            fs::read_to_string(state.join("brief.md")).unwrap(),
            "brief\r\n"
        );
        assert!(
            run(r"echo note>context\brief.md"),
            "the root context is the agent's to change once .crazyagent holds the state"
        );
        assert!(!git.exists());
    }

    #[test]
    fn a_job_folder_takes_top_level_entries_while_a_project_root_keeps_its_own() {
        let helper = helper();
        for job in [false, true] {
            let folders = folders();
            let project = &folders.project;
            let plain = policy(&folders, SandboxNetwork::Only(Vec::new()));
            let sandbox = if job { plain.as_job_folder() } else { plain };
            assert_eq!(sandbox.holds_project_guards(), !job);
            let guarded = boundary_profile(&sandbox, &folders.grants).unwrap().guarded;
            assert_eq!(
                guarded.iter().any(|item| item.guard == Guard::ReadOnly),
                !job,
                "only a project holds project guards"
            );
            assert!(guarded.iter().any(|item| item.guard == Guard::Hidden));
            let src = project.join("src");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("work.txt"), "work\r\n").unwrap();
            let profile = folders.profiles.join("agent.sb");
            let installed =
                WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
            let run = |line: &str| shell(&installed, &folders, line);
            assert!(
                run(r"echo changed>src\work.txt"),
                "the agent started: job {job}"
            );
            let top = project.join("top.txt");
            let moved = call(&installed, &folders, &[&src.join("work.txt"), &top]);
            if job {
                assert!(moved.status.success() && top.is_file(), "{moved:?}");
            } else {
                assert!(denied(&moved) && !top.exists(), "{moved:?}");
            }
            assert_eq!(run("echo new>notes.txt"), job, "job {job}");
            assert_eq!(run("mkdir docs"), job, "job {job}");
            assert_eq!(project.join("notes.txt").is_file(), job);
            assert_eq!(project.join("docs").is_dir(), job);
            assert_eq!(run(r"echo note>context\brief.md"), job, "job {job}");
            assert!(!project.join(".git").exists() && !project.join(".crazyagent").exists());
        }
    }

    /// #764 L1 on every write root: a turn profile rooted at a worktree, whose `.git` is a plain
    /// file, that also writes its repository or a folder above it could plant the repository's
    /// hooks, so it is refused; the same profile with the repository as a read root only builds.
    #[test]
    fn a_worktree_profile_that_also_writes_its_repository_is_refused() {
        let folders = folders();
        fs::create_dir_all(folders.project.join(".git").join("hooks")).unwrap();
        let worktree = folders.project.parent().unwrap().join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\r\n").unwrap();
        let turn = || {
            ProjectSandbox::new(
                &worktree,
                &folders.home,
                &folders.tmp,
                SandboxNetwork::Only(Vec::new()),
            )
            .and_then(|sandbox| sandbox.allow_read(&folders.project))
            .unwrap()
        };
        assert!(boundary_profile(&turn(), &folders.grants).is_ok());
        let above = folders.project.parent().unwrap().join("above");
        let nested = above.join("repository");
        fs::create_dir_all(nested.join(".git")).unwrap();
        for written in [&folders.project, &above] {
            let read = if written == &above {
                &nested
            } else {
                &folders.project
            };
            let wide = turn()
                .allow_read(read)
                .and_then(|sandbox| sandbox.allow_worktree(written))
                .unwrap();
            assert!(
                refused_for_git(boundary_profile(&wide, &folders.grants)),
                "{}",
                written.display()
            );
        }
    }

    /// Tells whether a profile build was refused because it writes the owner's git folder.
    /// Input: the build's result. Output: the answer.
    fn refused_for_git<T: std::fmt::Debug>(built: Result<T, SandboxError>) -> bool {
        matches!(built, Err(SandboxError::Invalid(reason)) if reason == WRITES_OWNER_GIT)
    }

    /// rv-sec3 #764 L1 (C6): a job folder on a repository writes its `.git` through its
    /// writable top, so an agent could plant a hook the engine's own git runs; its profile is
    /// refused at the chokepoint, at build and at install, and so is a job folder that is the
    /// git folder itself, while the same job folder on a folder with no git folder installs.
    #[test]
    fn a_job_folder_profile_on_a_repository_is_refused_because_it_writes_the_git_folder() {
        let folders = folders();
        let git = folders.project.join(".git");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::create_dir_all(git.join("refs")).unwrap();
        let job = policy(&folders, SandboxNetwork::Only(Vec::new())).as_job_folder();
        assert!(job.writes(&git.join("hooks")));
        assert!(refused_for_git(boundary_profile(&job, &folders.grants)));
        let helper = std::env::current_exe().unwrap();
        let profile = folders.profiles.join("job.sb");
        assert!(refused_for_git(WindowsSandbox::install(
            job,
            &helper,
            &profile,
            &folders.grants
        )));
        assert!(!profile.exists());
        let inside = ProjectSandbox::new(
            &git,
            &folders.home,
            &folders.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .as_job_folder();
        assert!(refused_for_git(boundary_profile(&inside, &folders.grants)));
        let plain = folders.project.parent().unwrap().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let elsewhere = ProjectSandbox::new(
            &plain,
            &folders.home,
            &folders.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .as_job_folder();
        assert!(boundary_profile(&elsewhere, &folders.grants).is_ok());
    }

    /// C6: the project guards hold `.git`'s hooks, config and modules, never its refs, objects
    /// or `HEAD`, so a project-root profile on a repository still writes the git folder and is
    /// refused too; a worktree's profile, whose `.git` is the file naming its admin folder and
    /// whose repository is read only, installs, as the master's turn builds it.
    #[test]
    fn a_guarded_project_root_profile_on_a_repository_is_refused_and_a_worktree_profile_is_not() {
        let folders = folders();
        let git = folders.project.join(".git");
        fs::create_dir_all(git.join("refs")).unwrap();
        let guarded = policy(&folders, SandboxNetwork::Only(Vec::new()));
        assert!(guarded.holds_project_guards());
        assert!(!guarded.protected_paths().contains(&git.join("refs")));
        assert!(refused_for_git(boundary_profile(&guarded, &folders.grants)));
        let worktree = folders.project.parent().unwrap().join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\r\n").unwrap();
        let turn = ProjectSandbox::new(
            &worktree,
            &folders.home,
            &folders.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .and_then(|sandbox| sandbox.allow_read(&folders.project))
        .unwrap();
        let profile = boundary_profile(&turn, &folders.grants).unwrap();
        assert!(
            !profile
                .write_roots
                .iter()
                .any(|root| Path::new(root).starts_with(&folders.project))
        );
    }
}
