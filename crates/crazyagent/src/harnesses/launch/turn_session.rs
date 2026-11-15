use super::super::drivers::jsonl_turn::Config;
use super::{LaunchError, LaunchEvent, LaunchRequest, Unstopped, failure};
use std::time::Duration;

/// A piped harness that runs one process per turn, on the `jsonl-turn` driver. Not built
/// yet: every start is refused with `launch_harness_unsupported`.
pub struct TurnSession {
    _request: LaunchRequest,
}

impl TurnSession {
    /// Starts a conversation with a catalog harness on the `jsonl-turn` driver. Input: the
    /// launch request. Output: `launch_harness_unsupported` until the driver lands.
    pub fn start(request: LaunchRequest) -> Result<Self, LaunchError> {
        drop(request);
        Err(failure("launch_harness_unsupported"))
    }

    /// Starts a conversation with the given jsonl-turn settings. Inputs: the launch request
    /// and the settings. Output: `launch_harness_unsupported` until the driver lands.
    pub fn start_with(request: LaunchRequest, config: Config) -> Result<Self, LaunchError> {
        drop((request, config));
        Err(failure("launch_harness_unsupported"))
    }

    /// The harness session id the next turn continues. Output: none.
    pub fn native_session(&self) -> Option<&str> {
        None
    }

    /// The process group of the running turn. Output: none.
    pub fn turn_group(&self) -> Option<u32> {
        None
    }

    /// Starts a turn. Input: the prompt. Output: `launch_harness_unsupported`.
    pub fn send(&mut self, prompt: &str) -> Result<(), LaunchError> {
        let _ = prompt;
        Err(failure("launch_harness_unsupported"))
    }

    /// Stops the running turn. Output: `launch_not_running`.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        Err(failure("launch_not_running"))
    }

    /// Waits up to `wait` for the next event. Output: none.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        let _ = wait;
        Ok(None)
    }

    /// Stops the session. Output: Ok.
    pub fn stop(self) -> Result<(), Unstopped> {
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "turn_session_tests.rs"]
mod tests;
