use super::{ServerInfo, Servers, Slot, failure, locked};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use kernel_sandbox::policy::AgentAccess;
use std::process::{Child, Command};
use std::sync::atomic::Ordering;

impl Servers {
    /// Starts a launch's server: refuses once [`Servers::stop_all`] began (a stop that begins
    /// after this check is caught by [`ready`]), issues the project's capability for the
    /// built-in extension at the launch's capability path, replacing any earlier one, then
    /// spawns through the kernel editor seam, which refuses a command not run through the
    /// sandbox runner where `required`; a spawn that fails revokes it again. Inputs: the
    /// project, its slot with no launch, the launch's info, its command and whether it must
    /// run confined. Output: the leader, or a plain refusal.
    pub(super) fn spawn(
        &self,
        project: &ProjectId,
        slot: &Slot,
        info: &ServerInfo,
        command: &mut Command,
        required: bool,
    ) -> Result<Child, EngineError> {
        {
            let state = locked(&slot.state);
            assert!(state.is_none());
            if self.closed.load(Ordering::SeqCst) {
                return Err(failure("ide_servers_closed"));
            }
        }
        let data = self
            .folder
            .parent()
            .ok_or_else(|| failure("ide_state_unavailable"))?;
        let capability = self.grants.issue(data, project)?;
        assert_eq!(capability, info.capability_path);
        let confinement = crate::sandbox::confinement();
        kernel_processes::editor::spawn(command, &confinement, required).map_err(|code| {
            let _ = self.grants.revoke(project);
            match code {
                kernel_processes::editor::NOT_CONFINED => failure(code),
                _ => failure("ide_server_start_failed"),
            }
        })
    }
}

/// Whether a launch's server must start through the sandbox runner: on a qualified system
/// at every access level but Full, which the owner chose to run unconfined. Input: the
/// launch's access level. Output: whether confinement is required.
pub(super) fn required(access: AgentAccess) -> bool {
    super::sandbox::qualified() && access != AgentAccess::Full
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
