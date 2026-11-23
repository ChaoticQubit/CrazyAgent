use crate::spawn_gate;
use crate::terminal::tmux::Confinement;
use std::ffi::OsStr;
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
    assert!(std::path::Path::new(confined.sandbox_exec).is_absolute());
    if required && command.get_program() != OsStr::new(confined.sandbox_exec) {
        return Err(NOT_CONFINED);
    }
    spawn_gate::spawn(command).map_err(|_| SPAWN_FAILED)
}

#[cfg(test)]
mod tests;
