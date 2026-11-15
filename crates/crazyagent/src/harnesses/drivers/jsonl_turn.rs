use super::super::launch::{LaunchError, LaunchEvent, failure};
use super::super::manifest::{self, Manifest, Refused};
use std::path::Path;

/// The most bytes of one argument a manifest writes for a turn, before it is filled.
pub const ARG_BYTES_MAX: usize = 1024;
/// The most bytes of a harness session id this driver keeps and passes back as an argument.
pub const SESSION_BYTES_MAX: usize = 256;
/// The most bytes of a model name this driver passes as an argument.
pub const MODEL_BYTES_MAX: usize = 256;

/// The piped settings of a harness on the `jsonl-turn` driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config;

impl Config {
    /// Reads the settings of one manifest. Input: the manifest. Output: a refusal until the
    /// driver lands.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let _ = manifest;
        Err(Refused {
            code: manifest::KIND,
            message: "piped.driver: the jsonl-turn driver has not landed.".into(),
        })
    }
}

/// Where a jsonl-turn conversation stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Failed,
}

/// One conversation with a harness that runs one process per turn. Until the driver lands
/// every call is refused as `launch_harness_unsupported`.
#[derive(Debug)]
pub struct JsonlTurn {
    phase: Phase,
}

impl JsonlTurn {
    /// Starts a conversation. Output: `launch_harness_unsupported` until the driver lands.
    pub fn new(
        config: Config,
        home: &Path,
        cwd: &Path,
        model: Option<&str>,
        tools: bool,
        resume: Option<&str>,
    ) -> Result<(Self, Vec<LaunchEvent>), LaunchError> {
        let _ = (config, home, cwd, model, tools, resume);
        Err(failure("launch_harness_unsupported"))
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id the next turn continues. Output: none until the driver lands.
    pub fn session(&self) -> Option<&str> {
        None
    }

    /// Starts a turn. Output: `launch_harness_unsupported` until the driver lands.
    pub fn start(&mut self, prompt: &str) -> Result<Vec<String>, LaunchError> {
        let _ = prompt;
        Err(failure("launch_harness_unsupported"))
    }

    /// Reads one line. Output: `launch_harness_unsupported` until the driver lands.
    pub fn receive(&mut self, line: &[u8]) -> Result<Vec<LaunchEvent>, LaunchError> {
        let _ = line;
        Err(failure("launch_harness_unsupported"))
    }

    /// Marks the turn interrupted. Output: `launch_harness_unsupported` until the driver
    /// lands.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }

    /// Ends the turn at its exit. Output: `launch_harness_unsupported` until the driver
    /// lands.
    pub fn exited(&mut self, code: Option<i32>) -> Result<Vec<LaunchEvent>, LaunchError> {
        let _ = code;
        Err(failure("launch_harness_unsupported"))
    }
}

#[cfg(test)]
#[path = "jsonl_turn_tests.rs"]
mod tests;
