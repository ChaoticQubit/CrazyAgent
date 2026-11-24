use crate::profile::command::SANDBOX_EXEC;
#[cfg(target_os = "macos")]
use crate::profile::command::{KEEPER, KEEPER_DEADLINE, KEEPER_STATE, PANE_SHIM, unkept_pane};
use crate::profile::env::agent_variable_allowed;

/// How the tmux host wraps a confined pane, built from this module's sandbox values: the
/// runner, the keeper and pane shim words and variables, and which variables reach a pane.
/// Output: the value the kernel tmux host is opened with.
pub fn confinement() -> kernel_processes::terminal::tmux::Confinement {
    kernel_processes::terminal::tmux::Confinement {
        sandbox_exec: SANDBOX_EXEC,
        #[cfg(target_os = "macos")]
        keeper: KEEPER,
        #[cfg(target_os = "macos")]
        keeper_deadline_variable: KEEPER_DEADLINE,
        #[cfg(target_os = "macos")]
        keeper_state_variable: KEEPER_STATE,
        #[cfg(target_os = "macos")]
        pane_shim: PANE_SHIM,
        variable_allowed: agent_variable_allowed,
        #[cfg(target_os = "macos")]
        unkept_pane,
    }
}
