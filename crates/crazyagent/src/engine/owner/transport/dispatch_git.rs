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

/// Starts a dispatch's Git child on a blocking thread within the read's timeout, which
/// counts from before the start, so a start held at the spawn gate never blocks a runtime
/// worker. A child started after the timeout is dropped as a `DispatchGit`, so it is killed
/// and reaped. Inputs: the configured command and the timeout. Output: the child and when
/// the timeout began, `dispatch_git_head_outcome_unknown` when the start outlasts it, or the
/// start's own error.
pub(super) async fn spawn_within(
    mut command: Command,
    timeout: Duration,
) -> Result<(DispatchGit, Instant), EngineError> {
    let started = Instant::now();
    let starting = tokio::task::spawn_blocking(move || {
        kernel_processes::spawn_gate::spawn(&mut command).map(DispatchGit)
    });
    let child = tokio::time::timeout(timeout, starting)
        .await
        .map_err(|_| failure("dispatch_git_head_outcome_unknown"))?
        .map_err(io_error)?
        .map_err(io_error)?;
    Ok((child, started))
}

#[cfg(test)]
#[path = "dispatch_git_tests.rs"]
mod tests;
