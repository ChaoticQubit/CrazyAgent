use super::super::launch::{LaunchError, LaunchEvent, failure};
use super::super::manifest::{self, Manifest, Refused, Rule};
use super::jsonl_turn::Phase;
use std::path::Path;

/// The most bytes of the prompt template or the interrupt frame a manifest writes.
pub const TEMPLATE_BYTES_MAX: usize = 1024;

/// The piped settings of a harness on the `jsonl-rpc` driver, read from its manifest: the
/// arguments of its one process and where their first `--` entry stands, the ones added to
/// continue a session, the ones added with tools off, the JSON line template each prompt is
/// framed in, the JSON line that interrupts a turn, and the rules that turn its output lines
/// into facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    argv: Vec<String>,
    positional: usize,
    resume: Vec<String>,
    tools_off: Option<Vec<String>>,
    prompt: String,
    interrupt: String,
    rules: Vec<Rule>,
}

impl Config {
    /// Reads the settings of one manifest. Input: a manifest whose `piped.driver` is
    /// `jsonl-rpc`. Output: the settings, or a refusal that names the setting.
    pub fn read(_manifest: &Manifest) -> Result<Self, Refused> {
        Err(Refused {
            code: manifest::KIND,
            message: String::new(),
        })
    }
}

/// One conversation with a harness that runs one long-lived process and speaks JSON lines
/// both ways, without any IO: it gives the process's arguments, frames each prompt and the
/// interrupt as one JSON line for its input, and turns its output lines into launch events
/// by the manifest's rules.
#[derive(Debug)]
pub struct JsonlRpc {
    config: Config,
    argv: Vec<String>,
    tools: bool,
    session: Option<String>,
    phase: Phase,
}

impl JsonlRpc {
    /// Starts a conversation. Inputs: the settings, the harness's private home and working
    /// folder, both absolute UTF-8, the model, the tools switch and the session to continue.
    /// Output: the conversation and its `Ready` event.
    pub fn new(
        _config: Config,
        _home: &Path,
        _cwd: &Path,
        _model: Option<&str>,
        _tools: bool,
        _resume: Option<&str>,
    ) -> Result<(Self, Vec<LaunchEvent>), LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }

    /// The arguments the harness's process starts with. Output: the filled arguments.
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id. Output: the id the harness last reported, else the one the
    /// conversation started from, or none.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Whether the running turn used a tool with tools off. Output: true from the breach
    /// until the turn ends.
    pub fn breached(&self) -> bool {
        false
    }

    /// Starts a turn. Input: the prompt. Output: the frame to write to the process.
    pub fn prompt(&mut self, _text: &str) -> Result<Vec<u8>, LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }

    /// Asks the running turn to stop. Output: the frame to write to the process.
    pub fn interrupt(&mut self) -> Result<Vec<u8>, LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }

    /// Reads one line the process wrote. Input: the line, with or without its newline.
    /// Output: the events it gives and the frames to write back.
    pub fn receive(
        &mut self,
        _line: &[u8],
    ) -> Result<(Vec<LaunchEvent>, Vec<Vec<u8>>), LaunchError> {
        Ok((Vec::new(), Vec::new()))
    }
}

#[cfg(test)]
#[path = "jsonl_rpc_tests.rs"]
mod tests;
