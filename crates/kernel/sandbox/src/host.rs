#[cfg(target_os = "linux")]
use crate::linux::launch as linux;
use crate::profile::{ProjectSandbox, linux_unbridged_notice_on};
use crate::settings::LinuxSupervisor;
use std::path::Path;

/// What this Linux holds for an agent beyond its Landlock file and network rules: `nested`
/// when it starts agents in a process namespace of their own, `scoped` when the kernel
/// keeps their signals inside their own Landlock domain (Linux 6.12 or newer),
/// `supervised` when the call supervisor holds every guard here, as launches start it,
/// `root` when CrazyAgent runs as root, where it starts no supervisor (rv-sec3 N1),
/// `armored` when AppArmor's user namespace restriction is why it nests none (SBX-3), and
/// `routed` when it gives an agent limited to ports on any host a network of its own (SBX-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxHost {
    pub nested: bool,
    pub scoped: bool,
    pub supervised: bool,
    pub root: bool,
    pub armored: bool,
    pub routed: bool,
}

impl LinuxHost {
    /// What this Linux holds under the owner's Linux call supervisor setting: supervised
    /// only while it is On, and run as root only while it is not Off, so the root line says
    /// why no supervisor runs. Input: the setting, none when it cannot be read, which shows
    /// every line. Output: the facts the access lines read; what is kept is never changed.
    pub fn under(self, calls: Option<LinuxSupervisor>) -> Self {
        assert!(!(self.root && self.supervised));
        let held = Self {
            supervised: self.supervised && calls == Some(LinuxSupervisor::On),
            root: self.root && calls != Some(LinuxSupervisor::Off),
            ..self
        };
        assert!(self.supervised || !held.supervised);
        assert!(!(held.root && held.supervised));
        held
    }
}

/// [`linux_unbridged_notice_on`] for this computer as it was last read, under the owner's
/// call supervisor setting ([`LinuxHost::under`]). Inputs: the project's network choice,
/// the agent's sandbox as launched and the setting, none when it cannot be read, which
/// counts as no supervisor. Output: the sentences, or none.
pub fn linux_unbridged_notice(
    choice: crate::policy::AgentNetwork,
    sandbox: &ProjectSandbox,
    calls: Option<LinuxSupervisor>,
) -> Option<String> {
    let host = linux_host_kept().map(|host| host.under(calls));
    linux_unbridged_notice_on(host, choice, sandbox)
}

/// What this computer holds as [`LinuxHost`] says, read through the sandbox helper its
/// launches take, again once what is kept is a minute old. A reader calls it before it opens
/// its database transaction, so no view inside one waits on a program. No input. Output:
/// none on another system; on Linux the facts, each false when it cannot be read, so its
/// gap line is shown.
pub fn linux_host() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host::host_now())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What this computer holds as [`linux_host`] last read it, however long ago, so a caller
/// inside a database transaction starts no helper; it reads only when nothing was read
/// yet. No input. Output: as [`linux_host`].
pub fn linux_host_kept() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host::host_kept())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Tells whether a project may hold a git repository besides its own top-level `.git`,
/// which an agent may still change under the call supervisor: on Linux what a bounded read
/// of the folder finds; elsewhere yes, where no Linux line shows. Input: the project
/// folder. Output: true when one may be there.
pub fn linux_other_git(project: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::host::holds_other_git(project)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = project;
        true
    }
}
