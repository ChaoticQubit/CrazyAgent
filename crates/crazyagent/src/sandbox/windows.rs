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

/// Why a Windows agent limited to its model endpoints is refused, in plain words.
pub const FIREWALL_RULE_MISSING: &str = "limiting a Windows agent to its model endpoints needs a firewall rule for its AppContainer, which this build does not have, so the agent does not start rather than reach more";

/// Why a Seatbelt profile is refused on Windows, in plain words.
pub const SEATBELT_MISSING: &str =
    "Windows has no Seatbelt; agents start through the AppContainer boundary";

/// The most processes one agent may run at once inside its job.
pub const ACTIVE_PROCESSES: u32 = 512;

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
/// of its own with read and execute on its read roots, modify on its writable roots, write
/// denied on the protected paths and every access denied on secret paths inside a granted
/// root, inside a job that kills the agent's whole tree when the helper ends.
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
    /// profile its next launch uses. Inputs: the policy, the helper and the profile path.
    /// Output: the sandbox, `Unavailable` for a missing helper or an endpoint limit, or
    /// `Invalid` for an unsafe path or policy.
    pub fn install(
        sandbox: ProjectSandbox,
        helper: &Path,
        profile: &Path,
    ) -> Result<Self, SandboxError> {
        let boundary = boundary_profile(&sandbox)?;
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
    let helper = existing_helper(&std::env::current_exe()?.with_file_name(HELPER_NAME))?;
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
    let mut hasher = Sha256::new();
    hasher.update(sandbox.project_root().as_os_str().as_encoded_bytes());
    hasher.update(b"\n");
    hasher.update(sandbox.agent_home().as_os_str().as_encoded_bytes());
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

/// The boundary profile for a policy: its container, network capabilities, read roots and
/// writable roots, the protected paths kept read-only and every secret path that is a plain
/// drive path hidden. The helper's own plan check runs here too, so a policy it would refuse
/// is refused at install. Input: the policy. Output: the profile, or the refusal.
pub fn boundary_profile(sandbox: &ProjectSandbox) -> Result<BoundaryProfile, SandboxError> {
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
    let mut guarded = sandbox
        .protected_paths()
        .iter()
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
    };
    profile
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    plan(&profile).map_err(invalid)?;
    Ok(profile)
}

fn drive_path(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .filter(|text| check_path(text).is_ok())
        .map(str::to_owned)
        .ok_or_else(|| invalid("a sandbox path is not a plain drive path"))
}

fn existing_helper(helper: &Path) -> Result<PathBuf, SandboxError> {
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
    use crate::sandbox::{InstalledSandbox, SandboxRemote};
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
    }

    fn folders() -> Folders {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let project = root.join("project");
        let home = root.join("agent").join("home");
        let tmp = root.join("agent").join("tmp");
        let profiles = root.join("profiles");
        for folder in [&project, &project.join("context"), &home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(project.join("context").join("brief.md"), "brief\r\n").unwrap();
        Folders {
            _temp: temp,
            project,
            home,
            tmp,
            profiles,
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
        let profile = boundary_profile(&sandbox).unwrap();
        assert_eq!(profile.capabilities, vec![Capability::InternetClient]);
        assert_eq!(
            profile.write_roots,
            [&folders.project, &folders.home, &folders.tmp]
                .map(|path| path.to_str().unwrap().to_owned())
        );
        for protected in sandbox.protected_paths() {
            assert!(profile.guarded.contains(&Guarded {
                path: protected.to_str().unwrap().to_owned(),
                guard: Guard::ReadOnly,
            }));
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
            WindowsSandbox::install(sandbox.clone(), &helper, &inside),
            Err(SandboxError::Invalid(_))
        ));
        assert!(!inside.exists());
        let missing = folders.profiles.join("missing-helper.exe");
        match WindowsSandbox::install(sandbox.clone(), &missing, &folders.profiles.join("a.sb")) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, APPCONTAINER_MISSING),
            other => panic!("expected the missing helper refusal, got {other:?}"),
        }
        let path = folders.profiles.join("agent.sb");
        let installed = WindowsSandbox::install(sandbox.clone(), &helper, &path).unwrap();
        let written = BoundaryProfile::load(&path).unwrap();
        assert_eq!(written, boundary_profile(&sandbox).unwrap());
        assert_eq!(installed.launcher(), helper.as_path());
        let limited = policy(
            &folders,
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        );
        match WindowsSandbox::install(limited, &helper, &folders.profiles.join("b.sb")) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, FIREWALL_RULE_MISSING),
            other => panic!("expected the firewall refusal, got {other:?}"),
        }
    }

    #[test]
    fn wrap_starts_the_harness_through_the_helper_with_pinned_folders() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let installed =
            WindowsSandbox::install(sandbox, &helper, &folders.profiles.join("agent.sb")).unwrap();
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
        match InstalledSandbox::install(sandbox, &profile) {
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
        let installed =
            WindowsSandbox::install(sandbox, &helper, &folders.profiles.join("agent.sb")).unwrap();
        let brief = folders.project.join("context").join("brief.md");
        let mut overwrite = Command::new(cmd());
        overwrite
            .arg("/C")
            .arg(format!("echo changed>{}", brief.display()))
            .current_dir(&folders.project);
        let status = Sandbox::wrap(&installed, &overwrite)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success());
        assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\r\n");
        let mut shell = Command::new(cmd());
        shell.arg("/K").current_dir(&folders.project);
        let mut running = Sandbox::wrap(&installed, &shell)
            .unwrap()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
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
}
