use crate::error::{SandboxError, invalid};
use crate::lines::MODEL_LOOPBACK_WINDOWS_LINE;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::paths::{canonical, clean_absolute};
use crate::profile::ProjectSandbox;
use crate::profile::command::Sandbox;
use crate::profile::env::{Handed, agent_process_env, agent_process_env_handing};
use crate::windows::{container_for, existing_helper, helper_beside_engine};
use crazyagent_win_sandbox::plan::plan;
use crazyagent_win_sandbox::profile::{BoundaryProfile, Capability, Guard, Guarded, check_path};
use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The most processes one agent may run at once inside its job.
pub const ACTIVE_PROCESSES: u32 = 512;
/// The lock file git writes beside `.git\config` while it changes the config. The Windows
/// boundary leaves it out of the protected paths: Windows cannot refuse creating one name, so
/// guarding a lock that is normally missing would keep `.git` from taking new entries, git's
/// own `index.lock` among them. A lock an agent makes never becomes the config: `config` is
/// read only, so no agent replaces or deletes it.
pub(crate) const GIT_CONFIG_LOCK: &str = "config.lock";

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
/// inside a job that kills the agent's whole tree when the helper ends.
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
        let grants = Some(grants)
            .filter(|grants| clean_absolute(grants) && grants.is_dir())
            .ok_or_else(|| invalid("grants folder must exist"))?;
        make_guarded_folders(&sandbox)?;
        let boundary = boundary_profile(&sandbox, &canonical(grants)?)?;
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

impl WindowsSandbox {
    /// Rebuilds a harness command as [`Sandbox::wrap`] does for a launch that hands its
    /// account over: its environment lets the names the exception lists hold its one value.
    /// Inputs: the harness command and the launch's exception. Output: the command, or the
    /// refusal.
    pub fn wrap_handing(
        &self,
        harness: &Command,
        handed: Handed<'_>,
    ) -> Result<Command, SandboxError> {
        self.wrapped(harness, Some(handed))
    }

    /// Rebuilds a harness command to start through the helper in this boundary, its
    /// environment built with the launch's exception to the variable guard when it has one.
    /// Inputs: the harness command and the exception, if any. Output: the command, or the
    /// refusal.
    fn wrapped(
        &self,
        harness: &Command,
        handed: Option<Handed<'_>>,
    ) -> Result<Command, SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness
            .get_current_dir()
            .unwrap_or(self.sandbox.project_root());
        if !clean_absolute(cwd)
            || !std::iter::once(self.sandbox.project_root())
                .chain(self.sandbox.worktrees().iter().map(PathBuf::as_path))
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
        let variables = harness.get_envs().filter(|&(name, _)| !pinned(name));
        let env = match handed {
            Some(handed) => agent_process_env_handing(&base, variables, handed)?,
            None => agent_process_env(&base, variables)?,
        };
        let mut command = boundary_command(&self.helper, &self.profile, program, &env);
        command.args(harness.get_args()).current_dir(cwd);
        assert_eq!(command.get_program(), self.helper.as_os_str());
        assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
        Ok(command)
    }
}

impl Sandbox for WindowsSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        self.wrapped(harness, None)
    }
}

/// Builds a Windows agent command without a bound sandbox: the helper next to the engine's
/// executable with `--profile <profile> -- <program>` and exactly [`agent_process_env`]. Inputs: an
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
    let env = agent_process_env(base, harness)?;
    let command = boundary_command(&helper, profile, program, &env);
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// The AppContainer capabilities for a network reach: the internet client
/// when open or limited to ports on any host, a limit the AppContainer cannot hold and so
/// widens; none when no endpoint is allowed; and [`MODEL_LOOPBACK_WINDOWS_LINE`] for any
/// loopback port, which it cannot reach. Input: the network. Output: the list or refusal.
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

/// The boundary profile for a policy: container, network capabilities, read and writable
/// roots, the protected paths kept read-only but git's config lock, and the project's root
/// `context` while `.crazyagent` is a real folder, since the state folder then holds the
/// context the engine reads ([`crate::access::AccessHooks::state_folder`]) and a missing root
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
    let unread = ((sandbox.hooks.access.state_folder)(root) != root).then(|| root.join("context"));
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
/// which would move a legacy project's state folder: the helper keeps the entries
/// of the folder above a missing guarded path, which would stop git's `index.lock`. A job
/// folder holds no guards, so none is made there. Input: the policy. Output: how many were
/// made, or the failure.
fn make_guarded_folders(sandbox: &ProjectSandbox) -> Result<usize, SandboxError> {
    if !sandbox.holds_project_guards() {
        return Ok(0);
    }
    let root = sandbox.project_root();
    let git = root.join(".git");
    let state = root.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
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

pub(crate) fn drive_path(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .filter(|text| check_path(text).is_ok())
        .map(str::to_owned)
        .ok_or_else(|| invalid("a sandbox path is not a plain drive path"))
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

pub(crate) fn system_root() -> String {
    std::env::var("SystemRoot")
        .ok()
        .filter(|root| check_path(root).is_ok())
        .unwrap_or_else(|| SYSTEM_ROOT_DEFAULT.to_owned())
}
