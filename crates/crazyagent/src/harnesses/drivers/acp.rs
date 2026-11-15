use super::super::launch::{LaunchError, LaunchEvent, failure};
use std::path::Path;

/// The Agent Client Protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// The JSON-RPC error code an ACP agent answers with when it needs its own sign-in first.
pub const AUTH_REQUIRED: i64 = -32000;
/// The JSON-RPC error code of a method this client does not serve.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The longest session id or request id this client keeps or echoes.
pub const ID_BYTES_MAX: usize = 256;

/// Where an ACP conversation stands: the handshake, the session being opened, waiting for a
/// prompt, a prompt turn, or failed for good after a refused line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Initializing,
    Opening,
    Idle,
    Prompting,
    Failed,
}

/// One ACP agent conversation over JSON-RPC lines. Until the client lands every call is
/// refused as `launch_harness_unsupported`.
#[derive(Debug)]
pub struct Acp {
    phase: Phase,
}

impl Acp {
    /// Starts a conversation. Inputs: the working folder, the tools switch and the session
    /// to continue. Output: `launch_harness_unsupported` until the client lands.
    pub fn new(
        cwd: &Path,
        tools: bool,
        resume: Option<&str>,
    ) -> Result<(Self, Vec<u8>), LaunchError> {
        let _ = (cwd, tools, resume);
        Err(failure("launch_harness_unsupported"))
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The agent's session id once a session is open. Output: none until the client lands.
    pub fn session(&self) -> Option<&str> {
        None
    }

    /// Starts a prompt turn. Input: the prompt text. Output: `launch_harness_unsupported`
    /// until the client lands.
    pub fn prompt(&mut self, text: &str) -> Result<Option<Vec<u8>>, LaunchError> {
        let _ = text;
        Err(failure("launch_harness_unsupported"))
    }

    /// Asks the agent to stop the running turn. Output: `launch_harness_unsupported` until
    /// the client lands.
    pub fn cancel(&mut self) -> Result<Option<Vec<u8>>, LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }

    /// Reads one line the agent wrote. Input: the line. Output:
    /// `launch_harness_unsupported` until the client lands.
    pub fn receive(
        &mut self,
        line: &[u8],
    ) -> Result<(Vec<LaunchEvent>, Vec<Vec<u8>>), LaunchError> {
        let _ = line;
        Err(failure("launch_harness_unsupported"))
    }
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;
