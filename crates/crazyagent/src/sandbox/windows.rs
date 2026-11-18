use super::{
    ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, agent_env, canonical, clean_absolute,
    invalid,
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

/// The boundary helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-win-boundary.exe";

/// Why a Windows launch is refused when the boundary helper is missing, in plain words.
pub const APPCONTAINER_MISSING: &str = "Windows agents start through crazyagent-win-boundary.exe next to the engine, which is missing, so no agent starts unconfined";

/// Why a Windows agent whose network is limited to a few ports is refused, in plain words.
pub const FIREWALL_RULE_MISSING: &str = "limiting a Windows agent's network to a few ports needs a firewall rule for its AppContainer, which this build does not have, so the agent does not start rather than reach more";

/// Why a Seatbelt profile is refused on Windows, in plain words.
pub const SEATBELT_MISSING: &str =
    "Windows has no Seatbelt; agents start through the AppContainer boundary";

/// The most processes one agent may run at once inside its job.
pub const ACTIVE_PROCESSES: u32 = 512;
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
    /// Writes the boundary profile for `sandbox` at `profile`, replacing any earlier one
    /// atomically, and binds it to `helper`. The profile must end in `.sb` like every
    /// platform's profile and lie outside every writable root, so no agent can rewrite the
    /// profile its next launch uses; the existing `grants` folder, where the helper records
    /// what it grants each container, must lie outside them too. Inputs: the policy, the
    /// helper, the profile path and the grants folder. Output: the sandbox, `Unavailable` for
    /// a missing helper or an endpoint limit, `Unreadable` for a read root no app container
    /// may read and this user cannot change ([`crazyagent_win_sandbox::unreadable_root`]),
    /// which the helper could not open to the agent, or `Invalid` for an unsafe path or
    /// policy. Before the profile, it makes each missing guarded folder (see
    /// [`make_guarded_folders`]).
    pub fn install(
        sandbox: ProjectSandbox,
        helper: &Path,
        profile: &Path,
        grants: &Path,
    ) -> Result<Self, SandboxError> {
        let grants = Some(grants)
            .filter(|grants| clean_absolute(grants) && grants.is_dir())
            .ok_or_else(|| invalid("grants folder must exist"))?;
        make_guarded_folders(&sandbox)?;
        let boundary = boundary_profile(&sandbox, &canonical(grants)?)?;
        if let Some(root) = boundary
            .read_roots
            .iter()
            .find(|root| crazyagent_win_sandbox::unreadable_root(Path::new(root)))
        {
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

/// The AppContainer capabilities for a network reach: the internet client capability when
/// the network is open, none when no endpoint is allowed, and `Unavailable` with
/// [`FIREWALL_RULE_MISSING`] for any endpoint limit, which is never widened to open.
/// Input: the network. Output: the capabilities, or the refusal.
pub fn capabilities(network: &SandboxNetwork) -> Result<Vec<Capability>, SandboxError> {
    match network {
        SandboxNetwork::Open => Ok(vec![Capability::InternetClient]),
        SandboxNetwork::Only(remotes) if remotes.is_empty() => Ok(Vec::new()),
        SandboxNetwork::Only(_) => Err(SandboxError::Unavailable(FIREWALL_RULE_MISSING.into())),
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
    fn the_network_reach_maps_to_capabilities_and_an_endpoint_limit_is_never_widened() {
        assert_eq!(
            capabilities(&SandboxNetwork::Open).unwrap(),
            vec![Capability::InternetClient]
        );
        assert!(
            capabilities(&SandboxNetwork::Only(Vec::new()))
                .unwrap()
                .is_empty()
        );
        for remote in [SandboxRemote::Loopback(8080), SandboxRemote::AnyHost(443)] {
            match capabilities(&SandboxNetwork::Only(vec![remote])) {
                Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, FIREWALL_RULE_MISSING),
                other => panic!("expected the firewall refusal, got {other:?}"),
            }
        }
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
        match WindowsSandbox::install(
            limited,
            &helper,
            &folders.profiles.join("b.sb"),
            &folders.grants,
        ) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, FIREWALL_RULE_MISSING),
            other => panic!("expected the firewall refusal, got {other:?}"),
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

    #[test]
    fn install_makes_each_missing_guarded_folder_and_leaves_what_is_there() {
        let folders = folders();
        let helper = std::env::current_exe().unwrap();
        let git = folders.project.join(".git");
        let state = folders.project.join(".crazyagent");
        fs::create_dir_all(&git).unwrap();
        fs::create_dir_all(&state).unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let first = folders.profiles.join("a.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &first, &folders.grants).unwrap();
        for folder in [
            git.join("hooks"),
            git.join("modules"),
            state.join("context"),
        ] {
            assert!(
                folder.is_dir() && fs::read_dir(&folder).unwrap().next().is_none(),
                "{} is made empty",
                folder.display()
            );
        }
        fs::write(git.join("hooks").join("pre-commit"), "owner\n").unwrap();
        fs::remove_dir(git.join("modules")).unwrap();
        fs::write(git.join("modules"), "owner\n").unwrap();
        let second = folders.profiles.join("b.sb");
        WindowsSandbox::install(sandbox, &helper, &second, &folders.grants).unwrap();
        assert_eq!(
            fs::read_to_string(git.join("hooks").join("pre-commit")).unwrap(),
            "owner\n"
        );
        assert_eq!(
            fs::read_to_string(git.join("modules")).unwrap(),
            "owner\n",
            "an entry already there is never replaced"
        );
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

    #[test]
    fn a_wrapped_agent_cannot_vacate_or_create_a_protected_path_but_still_edits_the_project() {
        let folders = folders();
        let helper = helper();
        let project = &folders.project;
        let git = project.join(".git");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::write(git.join("config"), "[core]\r\n").unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("README.md"), "readme\r\n").unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        let moved = call(&installed, &folders, &[&git, &project.join(".git-moved")]);
        assert!(denied(&moved), "{moved:?}");
        assert!(git.join("config").is_file() && !project.join(".git-moved").exists());
        assert!(!run(r"mkdir .crazyagent\context"));
        assert!(!project.join(".crazyagent").exists());
        assert!(run(r"echo planted>.git\config.lock"));
        let lock = git.join("config.lock");
        let replaced = call(&installed, &folders, &[&lock, &git.join("config")]);
        assert!(denied(&replaced), "{replaced:?}");
        assert_eq!(
            fs::read_to_string(git.join("config")).unwrap(),
            "[core]\r\n"
        );
        assert!(!run(r"mkdir .git\hooks\planted"));
        assert!(!git.join("hooks").join("planted").exists());
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

    #[test]
    fn a_wrapped_agent_without_a_checkout_adds_top_level_files_and_takes_git_locks() {
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
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        assert!(run(r"echo lock>.git\index.lock"));
        let lock = git.join("index.lock");
        let staged = call(&installed, &folders, &[&lock, &git.join("index")]);
        assert!(staged.status.success(), "{staged:?}");
        assert!(git.join("index").is_file() && !lock.exists());
        assert!(run("echo new>notes.txt"));
        assert!(run("mkdir docs"));
        assert!(project.join("notes.txt").is_file() && project.join("docs").is_dir());
        assert!(!run(r"echo planted>.git\hooks\pre-commit"));
        assert!(!git.join("hooks").join("pre-commit").exists());
        assert!(!run(r"echo planted>.crazyagent\context\brief.md"));
        let moved = call(&installed, &folders, &[&git, &project.join(".git-moved")]);
        assert!(denied(&moved), "{moved:?}");
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
        assert_eq!(
            fs::read_to_string(git.join("config")).unwrap(),
            "[core]\r\n"
        );
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
}
