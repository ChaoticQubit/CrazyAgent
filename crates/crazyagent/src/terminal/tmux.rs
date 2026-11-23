use kernel_processes::terminal::tmux::{TmuxHooks, installed_at};
use kernel_processes::terminal::{AgentId, HostError, LeadPaths};
use std::path::{Path, PathBuf};

/// Finds the tmux program the engine uses, anew on each call, through the one tool resolver:
/// the app's folder, the owner's own installs, the system folders, then `<root>/setup/bin`
/// last ([`kernel_processes::finding::Places::engine`]).
/// Takes the engine root. Returns the real program, or none when tmux is not installed.
pub fn installed(engine_root: &Path) -> Option<PathBuf> {
    installed_at(&crate::tools::tools_places(Some(engine_root)))
}

/// Where a native lead's sockets and records live in the team's state layout: the agent's
/// home, its short temp folder and the profile records. Output: the paths.
pub fn lead_paths() -> LeadPaths {
    LeadPaths {
        agent_home: crate::team::runtime::agent_home,
        tmp_path: kernel_sandbox::toolchains::tmp_path,
        agent_teammates_socket_name: crate::team::runtime::agent_teammates_socket_name,
        agent_teammates_server_pid: crate::team::runtime::agent_teammates_server_pid,
        agent_servers_deadline: crate::team::runtime::agent_servers_deadline,
    }
}

/// The team answers the kernel tmux host reads: the socket peer and the parent map from
/// the team caller, the agent profile and server state paths from the team runtime, and
/// the lead paths. Output: the hooks.
pub fn tmux_hooks() -> TmuxHooks {
    TmuxHooks {
        peer_pid: crate::team::caller::peer_pid,
        process_parents: crate::team::caller::process_parents,
        agent_profile: crate::team::runtime::agent_profile,
        agent_teammates_profile: crate::team::runtime::agent_teammates_profile,
        agent_servers_state: crate::team::runtime::agent_servers_state,
        lead: lead_paths(),
    }
}

/// The socket of a native-mode lead's own tmux server, as
/// [`kernel_processes::terminal::harness_socket`] names it in the team's layout. Takes the
/// engine root and the lead. Returns the path, or `terminal_agent_invalid`.
pub fn harness_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    kernel_processes::terminal::harness_socket(engine_root, lead, &lead_paths())
}

/// The socket of a native lead's teammates' server, as
/// [`kernel_processes::terminal::teammates_socket`] reads it in the team's layout. Takes the
/// engine root and the lead. Returns the path, or the host's refusal.
pub fn teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    kernel_processes::terminal::teammates_socket(engine_root, lead, &lead_paths())
}

/// Names a new teammates' socket for a native lead's start, as
/// [`kernel_processes::terminal::renew_teammates_socket`] does in the team's layout. Takes
/// the engine root and the lead. Returns the new path, or the host's refusal.
pub fn renew_teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    kernel_processes::terminal::renew_teammates_socket(engine_root, lead, &lead_paths())
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
