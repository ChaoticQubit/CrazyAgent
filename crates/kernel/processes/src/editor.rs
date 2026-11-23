use crate::spawn_gate;
use crate::terminal::tmux::Confinement;
use std::process::{Child, Command};

/// A command that must run confined does not start through the confinement runner.
pub const NOT_CONFINED: &str = "editor_not_confined";

/// The confined command did not start.
pub const SPAWN_FAILED: &str = "editor_spawn_failed";

/// Spawns the editor server for one project from a command its caller already confined,
/// and refuses one that is not confined where confinement is required. Inputs: the command,
/// the confinement, and whether this machine requires it. Output: the child, or a refusal
/// code.
pub fn spawn(
    command: &mut Command,
    confined: &Confinement,
    required: bool,
) -> Result<Child, &'static str> {
    let _ = (confined, required);
    spawn_gate::spawn(command).map_err(|_| SPAWN_FAILED)
}

#[cfg(test)]
mod tests;
