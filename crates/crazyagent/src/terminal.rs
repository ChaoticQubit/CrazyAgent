use crate::engine::EngineError;
use crate::harnesses::interactive::InteractiveCommand;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

pub mod buffer;
mod kept;
#[cfg(unix)]
pub mod tmux;
#[cfg(target_os = "windows")]
pub mod windows;

pub const FOLDER: &str = "terminal";
pub const AGENT_ID_BYTES_MAX: usize = 64;
pub const INPUT_BYTES_MAX: usize = 4096;
pub const PROMPT_BYTES_MAX: usize = 4096;
pub const CHUNK_BYTES_MAX: usize = 8192;
pub const SIZE_MIN: u16 = 2;
pub const SIZE_MAX: u16 = 1000;
pub const STREAM_CHUNKS_MAX: usize = 256;
pub const VIEWERS_MAX: usize = 8;
pub const VIEWER_BUFFER_BYTES_MAX: usize = 1024 * 1024;
pub const SCROLLBACK_LINES: u32 = 5000;
pub const PANES_PER_PROJECT_MAX: usize = crate::config::POOL_MEMBERS_MAX;

const _: () = assert!(PROMPT_BYTES_MAX == crate::team::DIRECT_INPUT_BYTES_MAX);
const _: () = assert!(CHUNK_BYTES_MAX * 4 + 256 < crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(VIEWER_BUFFER_BYTES_MAX >= CHUNK_BYTES_MAX * 16);
const _: () = assert!(PANES_PER_PROJECT_MAX == 15);

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
/// `history`, written first, puts the terminal's earlier rows into a fresh view's scrollback
/// and is empty when the host keeps none; `screen` then redraws the whole current screen in a
/// fresh terminal of `cols` by `rows`.
/// `next` gives each later output chunk in order and none once the terminal ends.
/// A viewer that falls more than `VIEWER_BUFFER_BYTES_MAX` behind loses the oldest output and
/// reads one `buffer::skipped_marker` chunk in its place.
#[derive(Debug)]
pub struct TerminalStream {
    pub agent: AgentId,
    pub cols: u16,
    pub rows: u16,
    pub history: Vec<u8>,
    pub screen: Vec<u8>,
    pub(crate) chunks: buffer::Chunks,
}

impl TerminalStream {
    /// Waits for the next output chunk. Takes the stream.
    /// Returns at most `CHUNK_BYTES_MAX` bytes, or none once the terminal ended.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.chunks.recv().await
    }

    /// Waits for the next output chunk outside an async runtime. Takes the stream.
    /// Returns at most `CHUNK_BYTES_MAX` bytes, or none once the terminal ended.
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

    /// Tells whether the app in an agent terminal reads keys itself yet: interactive apps
    /// turn on bracketed paste once their own input is live, and keys typed before that
    /// go through the terminal's line editing, which turns Enter into a newline.
    /// Takes the name. Returns true once it is on, and always for a host that cannot tell.
    fn input_live(&self, _agent: &AgentId) -> Result<bool, HostError> {
        Ok(true)
    }

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

    /// Ends every terminal except `keep`, as the engine quits.
    /// Takes the terminals that stay running, such as a master the owner keeps alive.
    /// Returns once the rest ended; with nothing kept, the host leaves nothing behind.
    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        end_except(self, keep)
    }
}

/// Ends every live terminal of a host that is not in `keep`.
/// Takes the host and the names to keep; a terminal that ended meanwhile is skipped.
/// Returns the first other failure.
pub fn end_except(host: &(impl Host + ?Sized), keep: &[AgentId]) -> Result<(), HostError> {
    for agent in host.alive()? {
        if keep.contains(&agent) {
            continue;
        }
        match host.kill(&agent) {
            Ok(()) => {}
            Err(problem) if problem.0 == "terminal_agent_unknown" => {}
            Err(problem) => return Err(problem),
        }
    }
    Ok(())
}

/// Leaves the host as the engine stops: team agent terminals keep running for the next
/// engine to take over, every other terminal ends, and with no team agent left the host
/// leaves nothing behind. Takes the host; one that cannot run has nothing to end.
/// Returns the host's first failure.
pub fn quit(host: &(impl Host + ?Sized)) -> Result<(), HostError> {
    let alive = match host.alive() {
        Ok(alive) => alive,
        Err(problem) if problem.0 == "terminal_unavailable" => return Ok(()),
        Err(problem) => return Err(problem),
    };
    let keep: Vec<AgentId> = alive
        .into_iter()
        .filter(|agent| agent.team_agent().is_some())
        .collect();
    host.close(&keep)
}

/// Turns a terminal code into the sentence clients show, such as for a refused attach.
/// Takes the code from `HostError`, `TerminalFrame::Rejected` or `TerminalFrame::Closed`.
/// Returns plain words without internal terms.
pub fn plain_reason(code: &str) -> &'static str {
    match code {
        "terminal_unavailable" => "Agent terminals need tmux. Run setup to install it.",
        "terminal_tmux_unsupported" => {
            "The installed tmux is too old for agent terminals. Run setup to update it."
        }
        "terminal_agent_unknown" | "terminal_ended" => {
            "This agent has stopped, so its terminal is closed."
        }
        "terminal_agent_running" => "This agent already has a terminal.",
        "terminal_agent_invalid" => "That is not an agent terminal.",
        "terminal_project_full" => {
            "This project already runs 15 agents, the most one project may run at once."
        }
        "terminal_viewers_full" => {
            "This terminal is open in too many places. Close one of them first."
        }
        "terminal_input_bounds" | "terminal_input_too_long" => {
            "That is too much to type at once. Send it in smaller parts."
        }
        "terminal_size_bounds" | "terminal_size_invalid" => "That terminal size is out of range.",
        "terminal_detached" => "You closed this view. The agent keeps running.",
        "terminal_viewer_gone" | "terminal_viewer_slow" => {
            "This view stopped reading, so it was closed. Open the terminal again."
        }
        "terminal_socket_path_long" | "terminal_folder_unsafe" => {
            "The engine's terminal folder cannot be used safely."
        }
        "terminal_not_answering" | "terminal_closed" | "terminal_attach_timeout" => {
            "The terminal host did not answer. Try again."
        }
        "owner_grant_stale_or_revoked" => {
            "This app's access to the engine changed. Open the terminal again."
        }
        _ => "The terminal could not be used.",
    }
}

/// Opens this platform's terminal host for an engine: the private tmux server on macOS and Linux,
/// keeping the last screen of each terminal it ends.
/// Takes the engine root; tmux keeps its socket in `<root>/terminal` and is looked for there first.
/// Returns whether the host can run now, and the host; one that cannot reports `terminal_unavailable`.
#[cfg(unix)]
pub fn platform(engine_root: &Path) -> (bool, Arc<dyn Host>) {
    let found = tmux::installed(engine_root);
    let ready = found.is_some();
    let host: Arc<dyn Host> = Arc::new(kept::LastScreenHost::new(tmux::Tmux::open(
        found,
        &engine_root.join(FOLDER),
    )));
    (ready, host)
}

/// Opens this platform's terminal host for an engine: the ConPTY host on Windows, keeping the
/// last screen of each terminal it ends.
/// Takes the engine root, unused because ConPTY needs no server or socket.
/// Returns that the host can run, and the host.
#[cfg(target_os = "windows")]
pub fn platform(_engine_root: &Path) -> (bool, Arc<dyn Host>) {
    let host: Arc<dyn Host> = Arc::new(kept::LastScreenHost::new(windows::ConptyHost::new()));
    (true, host)
}

/// What a viewer sends on an owner terminal connection.
/// The first frame is `Attach`; `Input`, `Prompt`, `Resize` and `Detach` follow on the same
/// connection. `Input` bytes, at most `INPUT_BYTES_MAX`, are typed as given and never kept.
/// `Prompt` text, at most `PROMPT_BYTES_MAX`, is the owner's explicit message: it is typed as
/// one submission, and a sub-agent's master later hears a note of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalRequest {
    Attach { agent_id: AgentId },
    Input { bytes: Vec<u8> },
    Prompt { text: String },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// The keys that type one owner prompt into an agent's app, before its Enter. Control
/// characters other than newline and tab are dropped, so the prompt cannot press keys or end
/// a paste. With bracketed paste on, the text goes as one paste with carriage returns between
/// lines, as a terminal pastes; with it off every line break would be an Enter, so lines join
/// with spaces. Takes the text and whether the app turned bracketed paste on.
/// Returns the keys, or none for blank text or text over `PROMPT_BYTES_MAX` bytes.
pub fn prompt_keys(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.len() > PROMPT_BYTES_MAX {
        return None;
    }
    let clean: String = text
        .replace("\r\n", "\n")
        .chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    let keys = if bracketed {
        format!("\x1b[200~{}\x1b[201~", clean.replace('\n', "\r"))
    } else {
        clean
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert!(!keys.contains('\n'));
    Some(keys.into_bytes())
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
