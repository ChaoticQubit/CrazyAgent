use crate::engine::EngineError;
use crate::harnesses::interactive::InteractiveCommand;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
pub mod tmux;

pub const AGENT_ID_BYTES_MAX: usize = 64;
pub const INPUT_BYTES_MAX: usize = 4096;
pub const CHUNK_BYTES_MAX: usize = 8192;
pub const SIZE_MIN: u16 = 2;
pub const SIZE_MAX: u16 = 1000;
pub const STREAM_CHUNKS_MAX: usize = 256;
pub const VIEWERS_MAX: usize = 8;

const _: () = assert!(INPUT_BYTES_MAX == crate::team::DIRECT_INPUT_BYTES_MAX);
const _: () = assert!(CHUNK_BYTES_MAX * 4 + 256 < crate::protocol::FRAME_BYTES_MAX);

/// A terminal host failure as a stable lowercase code clients show in plain words.
/// `terminal_unavailable` means the host cannot run at all, such as tmux missing.
/// Converts into the engine's own error with the same code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

impl From<HostError> for EngineError {
    fn from(error: HostError) -> Self {
        EngineError(error.0)
    }
}

fn failure(code: &str) -> HostError {
    HostError(code.into())
}

/// Names one agent's terminal: lowercase letters, digits and inner dashes, at most 64 bytes.
/// A team agent is `team-<id>`; other engine parts choose their own prefix.
/// The name survives engine restarts, so the host maps live terminals back to agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    /// Checks a terminal name. Takes the text.
    /// Returns the id, or `terminal_agent_invalid` for an empty, long or unsafe name.
    pub fn new(text: &str) -> Result<Self, HostError> {
        let bytes = text.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= AGENT_ID_BYTES_MAX
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && bytes[0] != b'-'
            && bytes[bytes.len() - 1] != b'-';
        if !valid {
            return Err(failure("terminal_agent_invalid"));
        }
        Ok(Self(text.to_owned()))
    }

    /// Names a team agent's terminal. Takes the team agent id.
    /// Returns `team-<id>`, or `terminal_agent_invalid` for an id below one.
    pub fn team(agent: i64) -> Result<Self, HostError> {
        if agent <= 0 {
            return Err(failure("terminal_agent_invalid"));
        }
        Self::new(&format!("team-{agent}"))
    }

    /// Reads the team agent behind a terminal name. Takes the id.
    /// Returns the team agent id for a `team-<id>` name, none for anything else.
    pub fn team_agent(&self) -> Option<i64> {
        let digits = self.0.strip_prefix("team-")?;
        let agent: i64 = digits.parse().ok()?;
        (agent > 0 && agent.to_string() == digits).then_some(agent)
    }

    /// Gives the name as text. Takes the id.
    /// Returns the checked name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentId {
    type Error = HostError;

    fn try_from(text: String) -> Result<Self, HostError> {
        Self::new(&text)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where one spawned agent runs: its name, the host's own pane reference and the process id.
/// The pane reference is opaque to callers, such as `%3` for tmux.
/// The process id is the pane's first process, the harness or its sandbox wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRef {
    pub agent: AgentId,
    pub pane: String,
    pub pid: u32,
}

/// One viewer's live view of an agent's terminal: its size, the screen at attach, then output.
/// `screen` redraws the whole current screen in a fresh terminal of `cols` by `rows`.
/// `next` gives each later output chunk in order and none once the terminal or viewer closes.
#[derive(Debug)]
pub struct TerminalStream {
    pub agent: AgentId,
    pub cols: u16,
    pub rows: u16,
    pub screen: Vec<u8>,
    pub(crate) chunks: tokio::sync::mpsc::Receiver<Vec<u8>>,
}

impl TerminalStream {
    /// Waits for the next output chunk. Takes the stream.
    /// Returns at most `CHUNK_BYTES_MAX` bytes, or none when the terminal ended or fell behind.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.chunks.recv().await
    }

    /// Waits for the next output chunk outside an async runtime. Takes the stream.
    /// Returns at most `CHUNK_BYTES_MAX` bytes, or none when the terminal ended or fell behind.
    pub fn next_blocking(&mut self) -> Option<Vec<u8>> {
        self.chunks.blocking_recv()
    }
}

/// Runs every agent's interactive app in a real terminal that outlives the engine and its viewers.
/// macOS and Linux use the engine's private tmux server; Windows has its own host behind this trait.
/// Every method fails with `terminal_unavailable` when the host cannot run on this machine.
pub trait Host: Send + Sync {
    /// Starts one agent's command in a new terminal named after the agent.
    /// Takes the name and the sandboxed command; fails with `terminal_agent_running` for a live name.
    /// Returns where it runs.
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError>;

    /// Opens one viewer on a live agent terminal.
    /// Takes the name; fails with `terminal_agent_unknown` when no such terminal runs.
    /// Returns the screen and the live output after it, with nothing lost or repeated between them.
    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError>;

    /// Types bytes into an agent terminal exactly as given, control keys included.
    /// Takes the name and at most `INPUT_BYTES_MAX` bytes.
    /// Returns once the host accepted them.
    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError>;

    /// Sets an agent terminal's size for its app and every viewer.
    /// Takes the name, columns and rows, each from `SIZE_MIN` to `SIZE_MAX`.
    /// Returns once the host applied it.
    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError>;

    /// Ends an agent terminal and the processes in it.
    /// Takes the name; an unknown name fails with `terminal_agent_unknown`.
    /// Returns once the terminal is gone.
    fn kill(&self, agent: &AgentId) -> Result<(), HostError>;

    /// Lists the agent terminals still running, reconnecting to the host first when needed.
    /// Takes nothing; after an engine restart this maps surviving terminals back to their agents.
    /// Returns the names in order.
    fn alive(&self) -> Result<Vec<AgentId>, HostError>;
}

/// What a viewer sends on an owner terminal connection.
/// The first frame is `Attach`; `Input`, `Resize` and `Detach` follow on the same connection.
/// Input bytes are at most `INPUT_BYTES_MAX`; a sub-agent's master later hears a note of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalRequest {
    Attach { agent_id: AgentId },
    Input { bytes: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// What the engine sends on an owner terminal connection.
/// `Attached` comes first with the size, then `Output` chunks: the screen, then live output.
/// `Closed` ends a stream that ran; `Rejected` ends one that never attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalFrame {
    Attached {
        agent_id: AgentId,
        cols: u16,
        rows: u16,
    },
    Output {
        bytes: Vec<u8>,
    },
    Closed {
        reason: String,
    },
    Rejected {
        code: String,
    },
}

#[cfg(test)]
#[path = "terminal/tests.rs"]
mod tests;
