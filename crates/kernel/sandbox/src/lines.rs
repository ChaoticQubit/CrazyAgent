use crate::error::SandboxError;
#[cfg(target_os = "linux")]
use crate::linux::launch as linux;
use crate::ports::BRIDGE_PORT_MIN;
use crate::profile::consts::LINUX_NOT_FULL;
#[cfg(target_os = "linux")]
use crate::profile::consts::{
    LINUX_HIDDEN_MOUNTED_AGAIN, LINUX_HIDDEN_MOUNTS_NOT_PRIVATE, LINUX_HIDDEN_NEEDS_NAMESPACES,
};

/// The Linux helper's exit code when its call supervisor could not take back an entry a
/// case-folding folder read as a held name, and so stopped the run. The supervisor crate
/// keeps its own copy, tied to this one in the Linux sandbox; 125 is its other stops, and
/// 126 and up are the shell's and the signals'.
pub const HELD_NOT_PUT_BACK_EXIT: i32 = 122;

/// Why `InstalledSandbox::install` refuses a Linux launch under `LinuxStrict::On`: it
/// does not hold the full Linux sandbox (`linux_holds_fully`).
pub const LINUX_NOT_FULL_LINE: &str = "this Linux does not hold the full sandbox for this launch, and the setting 'Refuse agents without the full Linux sandbox' is on";

/// The engine refusal code of a Windows launch whose limited network names a loopback port.
pub const MODEL_LOOPBACK_UNAVAILABLE_WINDOWS: &str = "model_loopback_unavailable_windows";

/// Why a Windows launch is refused when its network names a loopback port: the AppContainer cannot
/// reach this computer's own network without an admin exemption.
pub const MODEL_LOOPBACK_WINDOWS_LINE: &str = "a Windows agent cannot reach a port on this computer, such as a local model server or the relay, because its AppContainer has no way to this computer's own network";

/// Tells whether a launch refusal is the Windows loopback one ([`MODEL_LOOPBACK_WINDOWS_LINE`]),
/// so a caller can give it its own code ([`MODEL_LOOPBACK_UNAVAILABLE_WINDOWS`]). Input: the
/// refusal. Output: the answer.
pub fn model_loopback_windows(error: &SandboxError) -> bool {
    matches!(error, SandboxError::Unavailable(reason) if reason == MODEL_LOOPBACK_WINDOWS_LINE)
}

/// What a launch tells the owner in the agent's messages when this Linux nests agents but could not
/// give this one the network of its own its ports allow: why it has none, the AppArmor profile and
/// root included, and what that leaves open.
pub(crate) const LINUX_UNROUTED_LINE: &str = "On this Linux, this agent has no network of its own: slirp4netns or the nf_tables iptables is missing here, did not start or is kept out by this computer's AppArmor profile, or CrazyAgent runs as root, so every port it may reach is open to it on every host, this computer included.";

/// [`LINUX_UNROUTED_LINE`] for an agent with no loopback port, or one the call supervisor keeps to
/// this computer: its ports on any host then reach this computer too.
pub(crate) const LINUX_UNROUTED_ANY_HOST_LINE: &str = "On this Linux, this agent has no network of its own: slirp4netns or the nf_tables iptables is missing here, did not start or is kept out by this computer's AppArmor profile, or CrazyAgent runs as root, so the ports it may reach on any host include this computer's own.";

/// What a limited network cannot hold on Linux, shown there only, under the network choice in the
/// desktop and the TUI: Landlock limits outbound TCP by port, not by host, so a port an agent may
/// reach on this computer is open to it on every host.
pub const LINUX_PORTS_ANY_HOST_LINE: &str = "On Linux, network limits hold ports, not hosts: a port agents may reach on this computer, such as the model's proxy port or a port you allow, is open to them on every host.";

/// What a writable project folder cannot hold on a Linux where no call supervisor runs, among the
/// engine's lines under the access level in the desktop and the TUI: no Landlock rule stops an
/// agent from making a git repository inside a folder it may change.
pub const LINUX_NESTED_GIT_LINE: &str = "On Linux, agents can make a git repository inside any project folder they may change, and your own git or IDE runs its settings when you open that folder. Check agent-made folders for a .git before you open them.";

/// What a Linux agent can still do to the owner's other programs where this machine gives it
/// neither a process namespace of its own nor the Landlock signal scope (Linux before 6.12 without
/// usable user namespaces), shown there only, under the access level in the desktop and the TUI.
pub const LINUX_SIGNALS_LINE: &str = "On this Linux, agents can see which programs you run and stop or signal them, such as a dev server or CrazyAgent itself. Linux 6.12 or newer stops the signals.";

/// What a Linux agent can still see where the kernel scopes its signals but this machine gives it
/// no process namespace of its own, shown there only, under the access level in the desktop and the
/// TUI.
pub const LINUX_PROCESS_LIST_LINE: &str =
    "On this Linux, agents can see which programs you run and how they were started.";

/// What a writable project folder still cannot hold where the call supervisor runs: it refuses
/// every new `.git` entry but the agent's own, yet an agent may change a git repository already in
/// the project. Shown under the access level when the project may hold one (`linux_other_git`) and
/// in the New Project catalog.
pub const LINUX_KEPT_GIT_LINE: &str = "On Linux, agents can change a git repository already inside a project folder they may change, and your own git or IDE runs its settings when you open that folder. Check folders with a .git inside the project before you open them.";

/// What the call supervisor changes for agents where it runs: it makes the calls it guards for
/// them, so a program's own Landlock limits do not hold for those, and it refuses a create, even of
/// a file already there, under a `.git` the agent did not make.
pub const LINUX_SUPERVISED_LINE: &str = "On this Linux, CrazyAgent makes some calls for agents, such as making a file or folder or connecting, so a program an agent starts can't narrow those with limits of its own. In a .git folder CrazyAgent didn't make, agents can't create files and some writes to files already there are refused, so git commands that write fail in a repository nested in the project.";

/// Why no call supervisor runs where CrazyAgent runs as root: dumpability does not keep a root
/// agent away from it, so none starts and the lines after this one hold.
pub const LINUX_ROOT_LINE: &str = "CrazyAgent runs as root on this Linux, so it starts no call supervisor and agents can do what the lines below say. Run CrazyAgent as your own user to turn it on.";

/// What a limited network that reaches websites still cannot hold where the call supervisor keeps
/// ports on this computer to it: agents look up names over TCP, so port 53 is open to them on every
/// host.
pub const LINUX_DNS_ANY_HOST_LINE: &str =
    "On this Linux, agents can also reach port 53, used to look up names, on every host.";

/// Why agents share the owner's process space where AppArmor keeps unprivileged programs out of
/// user namespaces (Ubuntu 23.10 and newer) and this machine nests none, shown before the process
/// or signals line it explains, with the deb package that adds the profile allowing it.
pub const LINUX_APPARMOR_LINE: &str = "On this Linux, AppArmor keeps agents from getting a process space of their own. CrazyAgent's .deb package adds the AppArmor profile that allows it: install it, or if it is installed, run sudo dpkg-reconfigure crazy-agent.";

/// What the owner is told when a port below [`BRIDGE_PORT_MIN`] keeps agents on this
/// computer's network on a Linux that could give them one of their own: on the project's
/// Agent access page and in the agent's messages at its launch. Input: the port. Output:
/// the sentences, the port named.
pub fn linux_unbridged_line(port: u16) -> String {
    format!(
        "On this Linux, port {port} is below {BRIDGE_PORT_MIN}, so agents that may reach it share this computer's network: every port they may reach on this computer is open to them on every host. CrazyAgent keeps agents to this computer only for ports {BRIDGE_PORT_MIN} and up."
    )
}

/// The code a launch refusal has of its own, so a caller shows its plain line: the strict
/// setting's ([`LINUX_NOT_FULL`]), the Windows loopback's
/// ([`MODEL_LOOPBACK_UNAVAILABLE_WINDOWS`]) and, on Linux, the hidden folders' three
/// ([`LINUX_HIDDEN_NEEDS_NAMESPACES`], [`LINUX_HIDDEN_MOUNTED_AGAIN`],
/// [`LINUX_HIDDEN_MOUNTS_NOT_PRIVATE`]). Input: the refusal. Output: the code, or none.
pub fn refusal_code(error: &SandboxError) -> Option<&'static str> {
    let SandboxError::Unavailable(reason) = error else {
        return None;
    };
    match reason.as_str() {
        LINUX_NOT_FULL_LINE => Some(LINUX_NOT_FULL),
        MODEL_LOOPBACK_WINDOWS_LINE => Some(MODEL_LOOPBACK_UNAVAILABLE_WINDOWS),
        #[cfg(target_os = "linux")]
        linux::HIDDEN_NEEDS_NAMESPACES => Some(LINUX_HIDDEN_NEEDS_NAMESPACES),
        #[cfg(target_os = "linux")]
        linux::HIDDEN_MOUNTED_AGAIN => Some(LINUX_HIDDEN_MOUNTED_AGAIN),
        #[cfg(target_os = "linux")]
        crate::linux::helper::HIDDEN_MOUNTS_NOT_PRIVATE => Some(LINUX_HIDDEN_MOUNTS_NOT_PRIVATE),
        _ => None,
    }
}
