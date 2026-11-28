use crate::error::SandboxError;
#[cfg(target_os = "windows")]
use crate::error::invalid;
#[cfg(target_os = "linux")]
use crate::lines::LINUX_NOT_FULL_LINE;
#[cfg(target_os = "linux")]
use crate::linux::launch as linux;
#[cfg(target_os = "windows")]
use crate::paths::clean_absolute;
use crate::profile::ProjectSandbox;
use crate::profile::command::*;
use crate::profile::consts::*;
use crate::settings::{LinuxStrict, LinuxSupervisor};
#[cfg(target_os = "windows")]
use crate::windows::profile as windows;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux, AppContainer with a job
/// object on Windows.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
    unconfined: bool,
    calls: LinuxSupervisor,
    #[cfg(target_os = "linux")]
    linux: linux::LinuxSandbox,
    #[cfg(target_os = "windows")]
    windows: windows::WindowsSandbox,
}

impl InstalledSandbox {
    /// Installs the sandbox's profile at `profile` (see [`ProjectSandbox::install`]); on
    /// Linux, picks the Landlock helper next to the engine's executable or bubblewrap; on
    /// Windows, checks the helper next to the engine's executable first, then writes its
    /// boundary profile with the engine's grants folder (see
    /// [`windows::WindowsSandbox::install`]). `calls` and `strict` are the owner's "Linux call
    /// supervisor" and [`LinuxStrict`] choices, which every launch must pass and only Linux
    /// acts on. Output: the sandbox bound to that profile, the install refusal, or
    /// `Unavailable` where this system has no boundary.
    pub fn install(
        sandbox: ProjectSandbox,
        profile: &Path,
        calls: LinuxSupervisor,
        strict: LinuxStrict,
    ) -> Result<Self, SandboxError> {
        #[cfg(not(target_os = "linux"))]
        let _ = strict;
        if HIDDEN_UNHELD && !sandbox.hidden().is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.kept_out().is_empty() || sandbox.open_tops().is_some() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.shared_writes().is_empty() {
            return Err(SandboxError::Unavailable(
                SHARED_WRITES_NEED_SEATBELT.into(),
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if sandbox
            .agent_files()
            .is_some_and(|rule| rule.choice != crate::policy::IdeAgentFiles::AllowAll)
        {
            return Err(SandboxError::Unavailable(AGENT_FILES_NEED_SEATBELT.into()));
        }
        #[cfg(target_os = "windows")]
        {
            let grants = plain_grants((sandbox.hooks.grants_folder)()?)?;
            Self::install_with_grants(sandbox, profile, calls, &grants)
        }
        #[cfg(not(target_os = "windows"))]
        {
            sandbox.install(profile)?;
            assert!(profile.is_file());
            #[cfg(target_os = "linux")]
            let linux = {
                let helper = linux::helper_beside_engine()?;
                let data = kernel_processes::finding::engine_root();
                linux::LinuxSandbox::detect(sandbox.clone(), &helper)?.supervise(calls, data)
            };
            #[cfg(target_os = "linux")]
            if strict == LinuxStrict::On && !linux.holds_fully() {
                return Err(SandboxError::Unavailable(LINUX_NOT_FULL_LINE.into()));
            }
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
                calls,
                #[cfg(target_os = "linux")]
                linux,
            })
        }
    }

    /// The Windows arm of [`Self::install`], once its checks pass: the helper beside the engine and
    /// the boundary profile written with `grants`, which `install` gives as the engine's grants
    /// folder and a test as a folder of its own. Inputs: the policy, the profile path, the call
    /// supervisor choice and the grants folder, absolute and plain. Output: the boundary, or the
    /// refusal.
    #[cfg(target_os = "windows")]
    pub fn install_with_grants(
        sandbox: ProjectSandbox,
        profile: &Path,
        calls: LinuxSupervisor,
        grants: &Path,
    ) -> Result<Self, SandboxError> {
        assert!(grants.is_absolute(), "{}", grants.display());
        assert!(
            !grants.as_os_str().to_string_lossy().contains('/'),
            "{}",
            grants.display()
        );
        sandbox.secrets_still_clear()?;
        let helper = crate::windows::existing_helper(&crate::windows::helper_beside_engine()?)?;
        let windows = windows::WindowsSandbox::install(sandbox.clone(), &helper, profile, grants)?;
        assert!(profile.is_file());
        Ok(Self {
            sandbox,
            profile: profile.to_path_buf(),
            unconfined: false,
            calls,
            windows,
        })
    }

    /// Marks this boundary as a Full access project's: every command it wraps then starts with no
    /// Seatbelt profile (see [`ProjectSandbox::wrap_unconfined`]), keeping the agent environment,
    /// the worktree start and the process group. Only macOS runs Full access; elsewhere
    /// [`Self::wrap`] refuses an unconfined boundary. Output: the marked boundary.
    pub fn unconfined(mut self) -> Self {
        self.unconfined = true;
        self
    }

    /// Whether this boundary runs its commands with no Seatbelt profile.
    pub fn is_unconfined(&self) -> bool {
        self.unconfined
    }

    /// What a run's stop inside this boundary checks of the processes it left outside its
    /// group: its Full access lineage when unconfined, a sandboxed one on Linux where the
    /// backend leaves a process in a session of its own running (`LinuxSandbox::ends_every_session`),
    /// else nothing, as the warden, a process namespace or the job ends them all. Output: the
    /// check.
    pub fn lineage_check(&self) -> LineageCheck {
        if self.unconfined {
            return LineageCheck::FullAccess;
        }
        #[cfg(target_os = "linux")]
        if !self.linux.ends_every_session() {
            return LineageCheck::Sandboxed;
        }
        LineageCheck::Unneeded
    }

    /// The program every wrapped command starts: `sandbox-exec` on macOS, or the session
    /// shim's `perl` for a Full access boundary, the Landlock helper or bwrap on Linux, the
    /// boundary helper on Windows.
    pub fn launcher(&self) -> &Path {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            return Path::new(SESSION_SHIM[0]);
        }
        #[cfg(target_os = "linux")]
        {
            self.linux.launcher()
        }
        #[cfg(target_os = "windows")]
        {
            self.windows.launcher()
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Path::new(SANDBOX_EXEC)
        }
    }

    /// The sandbox this profile was generated from.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed profile file: the one `sandbox-exec` loads, or the boundary profile the
    /// Windows helper reads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// The owner's "Linux call supervisor" choice this boundary was installed with; only
    /// Linux acts on it.
    pub fn linux_supervisor(&self) -> LinuxSupervisor {
        self.calls
    }

    /// Rebuilds a harness command to run inside this boundary; see [`ProjectSandbox::wrap`]
    /// on macOS, [`linux::LinuxSandbox`] on Linux and [`windows::WindowsSandbox`] on Windows.
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if self.unconfined {
            return Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()));
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            let command = self.sandbox.wrap_unconfined(harness)?;
            assert_eq!(command.get_program(), self.launcher().as_os_str());
            return Ok(command);
        }
        #[cfg(target_os = "linux")]
        let command = Sandbox::wrap(&self.linux, harness)?;
        #[cfg(target_os = "windows")]
        let command = Sandbox::wrap(&self.windows, harness)?;
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let command = {
            let command = self.sandbox.wrap(&self.profile, harness)?;
            assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
            command
        };
        assert_eq!(command.get_program(), self.launcher().as_os_str());
        Ok(command)
    }

    /// Rebuilds a harness command as [`InstalledSandbox::wrap`] does for a launch that hands
    /// its account over, through [`linux::LinuxSandbox::wrap_handing`] on Linux and
    /// [`windows::WindowsSandbox::wrap_handing`] on Windows. Inputs: the harness command and
    /// the launch's exception. Output: the command, or the refusal.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn wrap_handing(
        &self,
        harness: &Command,
        handed: crate::profile::env::Handed<'_>,
    ) -> Result<Command, SandboxError> {
        if self.unconfined {
            return Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()));
        }
        #[cfg(target_os = "linux")]
        let command = self.linux.wrap_handing(harness, handed)?;
        #[cfg(target_os = "windows")]
        let command = self.windows.wrap_handing(harness, handed)?;
        assert_eq!(command.get_program(), self.launcher().as_os_str());
        Ok(command)
    }
}

impl Sandbox for InstalledSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        InstalledSandbox::wrap(self, harness)
    }
}

/// The engine's grants folder when it is a plain drive path, which `install_with_grants`
/// asserts: the data root comes from the owner's `CRAZYAGENT_HOME`, so `C:/data` or a UNC
/// root is refused here, as the boundary install refused it before `install_with_grants`
/// existed, never a panic.
/// Input: the folder. Output: the folder, or `Invalid`.
#[cfg(target_os = "windows")]
pub fn plain_grants(grants: PathBuf) -> Result<PathBuf, SandboxError> {
    assert!(!grants.as_os_str().is_empty());
    if !clean_absolute(&grants) {
        return Err(invalid("grants folder must exist"));
    }
    assert!(grants.is_absolute());
    Ok(grants)
}
