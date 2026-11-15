use super::super::launch::{LaunchError, LaunchEvent, failure};
use super::super::manifest::{self, Manifest, Refused};
use super::jsonl_turn::Phase;
use std::path::Path;

/// The most bytes of output one turn keeps; a turn whose output passes it is stopped.
pub const OUTPUT_BYTES_MAX: usize = 1024 * 1024;

/// The piped settings of a harness on the `text-turn` driver, read from its manifest: the
/// arguments of every turn, the ones added to continue a session, the ones added with tools
/// off, and, for a harness that ends its output with one JSON object, the paths of the
/// reply and of the session id in it. With no reply path, the reply is the whole output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    argv: Vec<String>,
    resume: Vec<String>,
    tools_off: Option<Vec<String>>,
    reply: Option<String>,
    session: Option<String>,
}

impl Config {
    /// Reads the settings of one manifest. Input: a manifest whose `piped.driver` is
    /// `text-turn`. Output: the settings, or a refusal that names the setting.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let _ = manifest;
        Err(Refused {
            code: manifest::KIND,
            message: "piped.driver: this harness does not run on the text-turn driver.".into(),
        })
    }
}

/// One conversation with a harness that runs one process per turn and gives no event
/// stream, without any IO: it gives each turn's arguments, keeps the turn's output within
/// [`OUTPUT_BYTES_MAX`], and when the process exits gives the reply, the whole output or one
/// field of its final JSON object, and the turn's end. It never reports usage.
#[derive(Debug)]
pub struct TextTurn {
    config: Config,
    phase: Phase,
}

impl TextTurn {
    /// Starts a conversation. Inputs: the settings, the harness's private home and working
    /// folder, the model and the tools switch. Output: the conversation and its `Ready`.
    pub fn new(
        config: Config,
        home: &Path,
        cwd: &Path,
        model: Option<&str>,
        tools: bool,
    ) -> Result<(Self, Vec<LaunchEvent>), LaunchError> {
        let _ = (config, home, cwd, model, tools);
        Err(failure("launch_harness_unsupported"))
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id the next turn continues. Output: the id, or none.
    pub fn session(&self) -> Option<&str> {
        let _ = &self.config;
        None
    }

    /// Whether the running turn's output passed [`OUTPUT_BYTES_MAX`], so its caller must stop
    /// the turn's process. Output: true from then until the turn ends.
    pub fn over(&self) -> bool {
        false
    }

    /// Starts a turn. Input: the prompt. Output: the turn's arguments.
    pub fn start(&mut self, prompt: &str) -> Result<Vec<String>, LaunchError> {
        let _ = prompt;
        Err(failure("launch_harness_unsupported"))
    }

    /// Keeps one line of the running turn's output. Input: the line, with its newline.
    /// Output: no events, as the reply comes when the process exits.
    pub fn receive(&mut self, line: &[u8]) -> Result<Vec<LaunchEvent>, LaunchError> {
        let _ = line;
        Ok(Vec::new())
    }

    /// Counts a line too long for the output reader as output past the bound.
    pub fn skipped(&mut self) {}

    /// Marks the running turn as interrupted; the caller then stops the turn's process.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        Err(failure("launch_not_running"))
    }

    /// Ends the running turn when its process exits. Input: the exit code, none for a
    /// signal. Output: the events.
    pub fn exited(&mut self, code: Option<i32>) -> Result<Vec<LaunchEvent>, LaunchError> {
        let _ = code;
        Err(failure("launch_not_running"))
    }
}

#[cfg(test)]
#[path = "text_turn_tests.rs"]
mod tests;
