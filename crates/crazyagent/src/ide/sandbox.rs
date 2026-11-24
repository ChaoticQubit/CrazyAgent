use crate::engine::EngineError;
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::policy::AgentFilesRule;
use std::path::{Path, PathBuf};

pub use kernel_sandbox::editor::{Grants, PROFILE, command, qualified};

/// The Seatbelt profile an IDE server runs under, built on the engine's hooks
/// ([`kernel_sandbox::editor::profile`]). Inputs: the grants, the server port, the network,
/// the ports kept and the agent folders rule. Output: the profile text, or
/// `ide_sandbox_invalid`.
pub fn profile(
    grants: &Grants,
    port: u16,
    network: SandboxNetwork,
    kept_ports: &[u16],
    agent_files: AgentFilesRule,
) -> Result<String, EngineError> {
    kernel_sandbox::editor::profile(
        &crate::sandbox::PROFILE_HOOKS,
        grants,
        port,
        network,
        kept_ports,
        agent_files,
    )
    .map_err(failure)
}

/// Writes an IDE server's profile on the engine's hooks
/// ([`kernel_sandbox::editor::install`]). Inputs: the folder, then as [`profile`]. Output:
/// the profile path, or `ide_sandbox_invalid` or `ide_state_unavailable`.
pub fn install(
    folder: &Path,
    grants: &Grants,
    port: u16,
    network: SandboxNetwork,
    kept_ports: &[u16],
    agent_files: AgentFilesRule,
) -> Result<PathBuf, EngineError> {
    kernel_sandbox::editor::install(
        &crate::sandbox::PROFILE_HOOKS,
        folder,
        grants,
        port,
        network,
        kept_ports,
        agent_files,
    )
    .map_err(failure)
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
