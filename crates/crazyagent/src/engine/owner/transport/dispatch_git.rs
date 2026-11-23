use crate::engine::EngineError;
use crate::engine::io_error;
use crate::engine::owner::failure;
use std::process::Command;
use std::time::{Duration, Instant};

/// The Git child that reads a dispatch's head, killed and reaped when dropped so no read
/// leaves it running.
pub(super) struct DispatchGit(pub(super) std::process::Child);

impl Drop for DispatchGit {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts a dispatch's Git child within the read's timeout, which counts from before the
/// start. Inputs: the configured command and the timeout. Output: the child and when the
/// timeout began, `dispatch_git_head_outcome_unknown` when the start outlasts it, or the
/// start's own error.
pub(super) async fn spawn_within(
    mut command: Command,
    timeout: Duration,
) -> Result<(DispatchGit, Instant), EngineError> {
    let started = Instant::now();
    let child = kernel_processes::spawn_gate::spawn(&mut command).map_err(io_error)?;
    if started.elapsed() >= timeout {
        return Err(failure("dispatch_git_head_outcome_unknown"));
    }
    Ok((DispatchGit(child), started))
}

#[cfg(test)]
#[path = "dispatch_git_tests.rs"]
mod tests;
