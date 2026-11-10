use crate::engine::EngineError;
use crate::harnesses::interactive::InteractiveCommand;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

pub mod buffer;
mod kept;
pub(crate) use kept::KEPT_MAX;
#[cfg(unix)]
pub mod tmux;
#[cfg(target_os = "windows")]
pub mod windows;

pub const FOLDER: &str = "terminal";
pub const HARNESS_SOCKET: &str = "tmux.sock";
/// The start of a native lead's teammates' socket name: each lead start names a new one,
/// `teammates-` and 16 hex digits then `.sock`, beside the lead's own.
pub const TEAMMATES_SOCKET_PREFIX: &str = "teammates-";
#[cfg(unix)]
const TEAMMATES_SOCKET_SUFFIX: &str = ".sock";
#[cfg(unix)]
const TEAMMATES_NONCE_BYTES: usize = 8;
#[cfg(unix)]
const TEAMMATES_NAME_BYTES: usize =
    TEAMMATES_SOCKET_PREFIX.len() + 2 * TEAMMATES_NONCE_BYTES + TEAMMATES_SOCKET_SUFFIX.len();
/// The most bytes a teammates' server pid record holds: the socket's name, a space and the
/// ten digits of the largest pid.
#[cfg(unix)]
const TEAMMATES_PIN_BYTES_MAX: usize = TEAMMATES_NAME_BYTES + 1 + 10;
/// What a teammates' server pid record holds after the socket's name until the server first
/// answers.
#[cfg(unix)]
const TEAMMATES_PIN_PENDING: &str = "pending";
const HARNESS_SOCKET_BYTES_MAX: usize = 104;
pub const AGENT_ID_BYTES_MAX: usize = 64;
pub const INPUT_BYTES_MAX: usize = 4096;
pub const PROMPT_BYTES_MAX: usize = 4096;
pub const CHUNK_BYTES_MAX: usize = 8192;
pub const SIZE_MIN: u16 = 2;
pub const SIZE_MAX: u16 = 1000;
pub const STREAM_CHUNKS_MAX: usize = 256;
pub const VIEWERS_MAX: usize = 8;
/// The most terminal views the desktop holds at once over every window, its main window and
/// the terminal wall windows together.
pub const DESKTOP_VIEWS_MAX: usize = 32;
/// The most terminal views the engine serves at once over every agent and client: the
/// desktop's views plus `VIEWERS_MAX` for the terminal app and other clients.
pub const CONNECTIONS_MAX: usize = VIEWERS_MAX + DESKTOP_VIEWS_MAX;
pub const VIEWER_BUFFER_BYTES_MAX: usize = 1024 * 1024;
pub const SCROLLBACK_LINES: u32 = 5000;
pub const PANES_PER_PROJECT_MAX: usize = crate::config::POOL_MEMBERS_MAX;
pub const PASTE_SETTLE: Duration = Duration::from_millis(200);

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

/// The most panes `Host::panes` lists for one agent terminal: the agent's own pane and the
/// panes its app split off, such as Claude Code agent team teammates.
pub const PANES_LISTED_MAX: usize = 32;
const PANE_ID_DIGITS_MAX: usize = 9;

/// One pane of an agent terminal's own window: the host's pane id, such as `%12`, the pane's
/// first process id and whether its process already ended while the pane stays open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    pub pid: u32,
    pub dead: bool,
}

/// Tells whether a text is a host pane id: `%` and one to nine digits. Takes the text,
/// which may come from a client. Returns true for a well formed id.
pub fn pane_id_valid(id: &str) -> bool {
    id.strip_prefix('%').is_some_and(|digits| {
        (1..=PANE_ID_DIGITS_MAX).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
    })
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

    /// Reads the visible screen of a live agent terminal without opening a viewer and
    /// without its scrollback. Takes the name; fails like `attach` for an unknown name.
    /// Returns the screen as `attach` draws it.
    fn screen(&self, agent: &AgentId) -> Result<Vec<u8>, HostError> {
        self.snapshot(agent).map(|stream| stream.screen)
    }

    /// Reads the size and visible screen of a live agent terminal without opening a viewer
    /// and without its scrollback. Takes the name; fails like `attach` for an unknown name.
    /// Returns them as a stream; a host that reads only through a viewer returns that viewer.
    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.attach(agent)
    }

    /// Tells whether this host still keeps the last screen of an agent terminal it ended,
    /// which `attach` then opens as a finished view with no live output after it. Takes the
    /// name. Returns false for a live or unknown name and always for a host that keeps none.
    fn kept(&self, _agent: &AgentId) -> bool {
        false
    }

    /// Keeps the last screen of an agent terminal that is about to end some other way than
    /// `kill`, as when its keeper empties its sandbox first, so `attach` still opens how it
    /// finished. Takes the name. Returns nothing; a host that keeps no screens, or a
    /// terminal already gone, keeps none.
    fn keep_last(&self, _agent: &AgentId) {}

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

    /// Reads how long an agent terminal's app has drawn nothing: a working harness redraws
    /// its spinner every second, so a quiet terminal means its app waits (DEC-104 Q13).
    /// Takes the name. Returns the time, or none when this host cannot tell, which keeps
    /// the engine from typing into that terminal.
    fn quiet(&self, _agent: &AgentId) -> Result<Option<Duration>, HostError> {
        Ok(None)
    }

    /// Sets an agent terminal's size for its app and every viewer.
    /// Takes the name, columns and rows, each from `SIZE_MIN` to `SIZE_MAX`.
    /// Returns once the host applied it.
    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError>;

    /// Ends an agent terminal and the processes in it.
    /// Takes the name; an unknown name fails with `terminal_agent_unknown`.
    /// Returns once the terminal is gone.
    fn kill(&self, agent: &AgentId) -> Result<(), HostError>;

    /// Lists the panes of an agent terminal's own window, at most `PANES_LISTED_MAX`, in
    /// the host's order. Takes the name; an unknown name fails with `terminal_agent_unknown`.
    /// Returns the panes; a host without split panes fails with `terminal_unavailable`.
    fn panes(&self, _agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Ends one pane of an agent terminal's own window and the processes in it, never the
    /// window's last pane. Takes the name and a pane id `panes` listed; a pane of another
    /// window fails with `terminal_pane_unknown`. Returns once the pane is gone.
    fn kill_pane(&self, _agent: &AgentId, _pane: &str) -> Result<(), HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Opens one viewer on one pane of an agent terminal's own window, such as a teammate
    /// its app split off. Takes the name and a pane id `panes` lists; a pane of another
    /// window fails with `terminal_pane_unknown`. Returns the stream as `attach` does.
    fn attach_pane(&self, _agent: &AgentId, _pane: &str) -> Result<TerminalStream, HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Reads one pane's size and visible screen without opening a viewer. Takes the name and
    /// the pane id. Returns them as `snapshot` does, or fails as `attach_pane` does.
    fn snapshot_pane(&self, _agent: &AgentId, _pane: &str) -> Result<TerminalStream, HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Types bytes into one pane exactly as given. Takes the name, the pane id and at most
    /// `INPUT_BYTES_MAX` bytes. Returns once the host accepted them.
    fn input_pane(&self, _agent: &AgentId, _pane: &str, _bytes: &[u8]) -> Result<(), HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Tells whether the app in one pane reads keys itself yet, as `input_live` does for the
    /// agent's own pane. Takes the name and the pane id. Returns the answer.
    fn input_live_pane(&self, _agent: &AgentId, _pane: &str) -> Result<bool, HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Gives a native teammate's pane its own name (DEC-104 K5a), so later calls by that
    /// name, `kill` included, reach that one pane of its lead's own server. Calling it again
    /// for the same pane changes nothing, so discovery may repeat it each pass and an engine
    /// restart relearns the name. Takes the teammate, its lead and a pane id the lead's
    /// `panes` lists other than the lead's own. Returns Ok, `terminal_pane_unknown`, or
    /// `terminal_own_server_unavailable` when the lead has no server of its own.
    fn adopt_pane(&self, _agent: &AgentId, _lead: &AgentId, _pane: &str) -> Result<(), HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Starts a native-mode lead in a tmux server of its own, started inside the Seatbelt
    /// profile its sandboxed command names, so every pane it splits off for a teammate runs
    /// under that profile and the lead never reaches the engine's own server. Takes the
    /// name and the command. Returns where it runs, or `terminal_own_server_unavailable`
    /// where no such server can run.
    fn spawn_own_server(
        &self,
        _agent: &AgentId,
        _command: &InteractiveCommand,
    ) -> Result<PaneRef, HostError> {
        Err(failure(OWN_SERVER_UNAVAILABLE))
    }
    /// Names the first process of a live agent terminal's own pane as the host reads it while
    /// the terminal runs, so a caller holding no session for it can end what its harness left:
    /// on a lead's server the pane tagged with the agent's name, else its window's first pane.
    /// Takes the name; an unknown name fails with `terminal_agent_unknown`. Returns the
    /// process id, or none for a host that cannot tell.
    fn pane_pid(&self, _agent: &AgentId) -> Result<Option<u32>, HostError> {
        Ok(None)
    }

    /// Whether this host runs an agent's pane without the engine's pane keeper, as a native
    /// lead's own server does, which already runs inside the lead's sandbox: no warden then
    /// ends what the pane's agent started, so its stop is judged by the processes still found.
    /// Takes the name. Returns the answer; false for a host that runs each pane as given.
    fn unkept(&self, _agent: &AgentId) -> bool {
        false
    }

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

    /// Moves the time each server of this host ends at once no engine renews it (FBTMUX-P1),
    /// so a server whose engine never comes back does not run forever. Takes the deadline.
    /// Returns the first failure; a host with no server outliving the engine has nothing to
    /// renew.
    fn renew(&self, _until: SystemTime) -> Result<(), HostError> {
        Ok(())
    }
}

/// The refusal for a native-mode lead on a host that cannot start a tmux server inside the
/// lead's sandbox: Linux, Windows, or a missing tmux.
pub const OWN_SERVER_UNAVAILABLE: &str = "terminal_own_server_unavailable";

/// The refusal for a team pane call on a native lead whose latest start named a teammates'
/// server that no longer accepts connections; its pane ids are never read on another server.
pub const TEAMMATES_SERVER_ENDED: &str = "terminal_teammates_server_ended";

/// The refusal for a native lead's teammates' socket answered by a server under another pid
/// than the one the engine first reached there (PLC c).
pub const TEAMMATES_SERVER_REPLACED: &str = "terminal_teammates_server_replaced";

/// A host that starts each agent through `Host::spawn_own_server`, in a tmux server of its
/// own inside the agent's sandbox; every other call goes to the host unchanged.
struct OwnServer(Arc<dyn Host>);

/// Wraps a host so the agents it spawns start as native-mode leads, each in its own tmux
/// server inside its sandbox (see `Host::spawn_own_server`). Takes the engine's host.
/// Returns the wrapped host; the engine routes every later call to the lead's own server.
pub fn own_server(host: Arc<dyn Host>) -> Arc<dyn Host> {
    Arc::new(OwnServer(host))
}

impl Host for OwnServer {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        self.0.spawn_own_server(agent, command)
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.0.attach(agent)
    }

    fn screen(&self, agent: &AgentId) -> Result<Vec<u8>, HostError> {
        self.0.screen(agent)
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.0.snapshot(agent)
    }

    fn keep_last(&self, agent: &AgentId) {
        self.0.keep_last(agent)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.0.input(agent, bytes)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.0.input_live(agent)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        self.0.quiet(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.0.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        self.0.kill(agent)
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        self.0.panes(agent)
    }

    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        self.0.pane_pid(agent)
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.0.unkept(agent)
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        self.0.kill_pane(agent, pane)
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.0.attach_pane(agent, pane)
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.0.snapshot_pane(agent, pane)
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.0.input_pane(agent, pane, bytes)
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        self.0.input_live_pane(agent, pane)
    }

    fn adopt_pane(&self, agent: &AgentId, lead: &AgentId, pane: &str) -> Result<(), HostError> {
        self.0.adopt_pane(agent, lead, pane)
    }

    fn spawn_own_server(
        &self,
        agent: &AgentId,
        command: &InteractiveCommand,
    ) -> Result<PaneRef, HostError> {
        self.0.spawn_own_server(agent, command)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.0.alive()
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        self.0.close(keep)
    }

    fn renew(&self, until: SystemTime) -> Result<(), HostError> {
        self.0.renew(until)
    }
}

/// A view of one pane of an agent terminal's own window, such as a native teammate's, as a
/// terminal of its own: viewing, typing and reading go to that pane, a resize sizes the
/// window the pane sits in, and ending it ends only that pane, never the window's last.
struct PaneView {
    host: Arc<dyn Host>,
    pane: String,
}

/// Opens a view of one pane of an agent terminal's own window as a terminal of its own, so
/// a viewer attaches, types and resizes it as it would an agent's terminal. Takes the host
/// and a pane id. Returns the view, or `terminal_pane_unknown` for a malformed id.
pub fn pane_view(host: Arc<dyn Host>, pane: &str) -> Result<Arc<dyn Host>, HostError> {
    if !pane_id_valid(pane) {
        return Err(failure("terminal_pane_unknown"));
    }
    Ok(Arc::new(PaneView {
        host,
        pane: pane.to_owned(),
    }))
}

impl Host for PaneView {
    fn spawn(&self, _agent: &AgentId, _command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        Err(failure("terminal_unavailable"))
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.host.attach_pane(agent, &self.pane)
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.host.snapshot_pane(agent, &self.pane)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.host.input_pane(agent, &self.pane, bytes)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.host.input_live_pane(agent, &self.pane)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        self.host.quiet(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.host.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        self.host.kill_pane(agent, &self.pane)
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.host.unkept(agent)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.host.alive()
    }

    fn close(&self, _keep: &[AgentId]) -> Result<(), HostError> {
        Ok(())
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

/// Leaves the host as the engine stops: while the owner keeps team agents running when
/// CrazyAgent quits, their terminals keep running for the next engine to take over and every
/// other terminal ends; otherwise every terminal ends. With no team agent kept the host
/// leaves nothing behind. Takes the host, one that cannot run having nothing to end, and
/// whether team agents keep running ([`crate::quit_settings::QuitSettings`]). Returns the
/// host's first failure.
pub fn quit(host: &(impl Host + ?Sized), keep_team_agents: bool) -> Result<(), HostError> {
    let alive = match host.alive() {
        Ok(alive) => alive,
        Err(problem) if problem.0 == "terminal_unavailable" => return Ok(()),
        Err(problem) => return Err(problem),
    };
    let keep: Vec<AgentId> = alive
        .into_iter()
        .filter(|agent| keep_team_agents && agent.team_agent().is_some())
        .collect();
    host.close(&keep)
}

#[cfg(unix)]
/// How often a running engine moves its terminal servers' deadlines ahead (FBTMUX-P1).
pub const RENEW_EVERY: Duration = Duration::from_secs(60);

#[cfg(unix)]
/// The engine's renewal of its terminal servers' deadlines while it runs (FBTMUX-P1). Each
/// pass reads the owner's agents on quit setting again and sets every server's deadline that
/// far ahead, so the holder in a server whose engine never comes back ends it once the time
/// is up, and an engine that comes back in time renews it before adopting its agents.
pub struct Renewal {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
/// Starts renewing the deadlines of a host's servers at once and every [`RENEW_EVERY`].
/// A pass whose setting cannot be read renews with the default setting and says why (see
/// [`renew_once`]). Takes the host and the engine root that holds the engine database.
/// Returns the running renewal, or None when its thread cannot start.
pub fn renew_while_running(host: Arc<dyn Host>, root: PathBuf) -> Option<Renewal> {
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = stop.clone();
    let thread = std::thread::Builder::new()
        .name("terminal-renewal".into())
        .spawn(move || {
            let (stopped, wake) = &*signal;
            loop {
                if let Err(problem) = renew_once(host.as_ref(), &root, SystemTime::now()) {
                    eprintln!(
                        "terminal_deadline_not_renewed cause={}",
                        crate::protocol::plain_code(&problem.0)
                    );
                }
                let guard = stopped
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let (guard, _) = wake
                    .wait_timeout_while(guard, RENEW_EVERY, |stop| !*stop)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *guard {
                    return;
                }
            }
        })
        .ok()?;
    Some(Renewal {
        stop,
        thread: Some(thread),
    })
}

#[cfg(unix)]
/// Renews the deadlines of a host's servers once from the owner's saved setting, or from the
/// default setting when the saved one cannot be read, logged, so a running engine never lets
/// its own servers reach a deadline it set earlier (FBTMUX-P1 r1 L2). Takes the host, the
/// engine root and the current time. Returns Ok, or the host's first failure.
pub fn renew_once(host: &dyn Host, root: &Path, now: SystemTime) -> Result<(), EngineError> {
    let settings = saved_quit_settings(root).unwrap_or_else(|problem| {
        eprintln!("terminal_deadline_from_default cause={}", problem.0);
        crate::quit_settings::QuitSettings::default()
    });
    host.renew(settings.deadline(now))
        .map_err(|problem| EngineError(problem.0))
}

#[cfg(unix)]
/// Reads the owner's agents on quit setting from the engine database. Takes the engine root.
/// Returns the setting, or the database failure or `quit_settings_invalid`.
fn saved_quit_settings(root: &Path) -> Result<crate::quit_settings::QuitSettings, EngineError> {
    let db = crate::landing::open(root)?;
    crate::quit_settings::initialize(&db)?;
    crate::quit_settings::load(&db)
}

#[cfg(unix)]
impl Renewal {
    /// Stops renewing and waits for the thread; the deadlines last set stay. Takes the
    /// renewal.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
impl Drop for Renewal {
    fn drop(&mut self) {
        self.halt();
    }
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
        "terminal_pane_unknown" => "That teammate's terminal has already closed.",
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
        "terminal_reply_too_large" => {
            "This terminal holds too much to show at once. The agent keeps running."
        }
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
    (found.is_some(), tmux_host(engine_root, found))
}

/// Opens the engine's terminal host on one tmux program, keeping the last screen of each
/// terminal it ends: the engine's private server, and each native-mode lead's own server
/// inside that lead's sandbox (see [`harness_socket`]). Takes the engine root and the
/// program, none when tmux is missing. Returns the host; nothing starts until its first call.
#[cfg(unix)]
pub fn tmux_host(engine_root: &Path, program: Option<PathBuf>) -> Arc<dyn Host> {
    let engine = tmux::Tmux::open(program.clone(), &engine_root.join(FOLDER));
    Arc::new(kept::LastScreenHost::new(tmux::Servers::new(
        engine,
        engine_root,
        program,
    )))
}

/// The socket of a native-mode lead's own tmux server: `tmux.sock` in the lead's short private
/// temp folder, which only that lead's sandbox may bind or reach, never the engine's server.
/// The engine reaches it only through a tmux client started inside the lead's own profile,
/// since a tmux server can make any client run a command; never connect a plain client.
/// Takes the engine root and the lead. Returns the path, or `terminal_agent_invalid` for a
/// name that is not a team agent.
pub fn harness_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    lead_temp_socket(engine_root, lead, HARNESS_SOCKET)
}

/// The socket of a native lead's teammates' own tmux server (rv-audit S1 F1), in the lead's
/// short private temp folder under the name its latest start chose (see
/// [`renew_teammates_socket`]). That server and every teammate pane it forks run in the
/// teammates' profile, which serves this socket but never reaches or unlinks it, nor the
/// lead's own; the lead and the engine's clients reach it inside the lead's profile. Takes
/// the engine root and the lead. Returns the path, `terminal_agent_invalid` for a name that
/// is not a team agent, or `terminal_own_server_unavailable` when no plain name is kept.
#[cfg(unix)]
pub fn teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    use std::os::unix::fs::MetadataExt;
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let kept = crate::team::runtime::agent_teammates_socket_name(&engine_root.join("team"), id);
    let unavailable = || failure(OWN_SERVER_UNAVAILABLE);
    let plain = std::fs::symlink_metadata(&kept).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.uid() == rustix::process::getuid().as_raw()
            && u64::try_from(TEAMMATES_NAME_BYTES).is_ok_and(|bytes| metadata.len() == bytes)
    });
    if !plain {
        return Err(unavailable());
    }
    let name = std::fs::read_to_string(&kept).map_err(|_| unavailable())?;
    if !teammates_name_valid(&name) {
        return Err(unavailable());
    }
    lead_temp_socket(engine_root, lead, &name)
}

/// Names a new socket for a native lead's teammates' server as the lead starts and keeps
/// the name in the engine's own profiles folder, so no path a teammate's profile ever
/// served is reused: a teammate still running from an earlier start cannot bind a listener
/// where the lead's tmux clients now connect. The pin record is marked pending for the new
/// name in the same step, so one that later goes missing or names another socket is refused
/// (PLCC-L1). Takes the engine root and the lead, whose profiles folder must exist. Returns
/// the new path, or a plain code.
#[cfg(unix)]
pub fn renew_teammates_socket(engine_root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let mut nonce = [0_u8; TEAMMATES_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| failure(OWN_SERVER_UNAVAILABLE))?;
    let digits: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = format!("{TEAMMATES_SOCKET_PREFIX}{digits}{TEAMMATES_SOCKET_SUFFIX}");
    assert!(teammates_name_valid(&name));
    keep_record(
        &crate::team::runtime::agent_teammates_socket_name(&engine_root.join("team"), id),
        &name,
    )?;
    let pending = format!("{name} {TEAMMATES_PIN_PENDING}");
    assert!(pending.len() <= TEAMMATES_PIN_BYTES_MAX);
    keep_record(
        &crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id),
        &pending,
    )?;
    lead_temp_socket(engine_root, lead, &name)
}

/// Writes one engine record in the profiles folder no agent writes, in place of any earlier
/// one: a staged file only this user reads, renamed over the record so no reader sees part
/// of it. Takes the record path and its text. Returns Ok, or
/// `terminal_own_server_unavailable`.
#[cfg(unix)]
fn keep_record(kept: &Path, text: &str) -> Result<(), HostError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let staging = kept.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&staging)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
        })
        .and_then(|()| std::fs::rename(&staging, kept));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
        return Err(failure(OWN_SERVER_UNAVAILABLE));
    }
    Ok(())
}

/// The pid a native lead's teammates' server at one socket was pinned to (PLC c, PC-L1),
/// read without following a link from the record beside the socket's name, which outlives
/// the engine and a kill of the lead. Takes the engine root, the lead and the socket.
/// Returns the pid, none while the record is pending for this socket, or
/// `terminal_own_server_unavailable` for a record that is missing, names another socket or
/// is not this user's plain file as [`renew_teammates_socket`] and
/// [`keep_teammates_server_pid`] write one, so such a record refuses every server (PLCC-L1).
#[cfg(unix)]
fn teammates_server_pid(
    engine_root: &Path,
    lead: &AgentId,
    socket: &Path,
) -> Result<Option<u32>, HostError> {
    use std::os::unix::fs::MetadataExt;
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let kept = crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id);
    let unavailable = || failure(OWN_SERVER_UNAVAILABLE);
    let metadata = std::fs::symlink_metadata(&kept).map_err(|_| unavailable())?;
    let plain = metadata.is_file()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && u64::try_from(TEAMMATES_PIN_BYTES_MAX).is_ok_and(|bytes| metadata.len() <= bytes);
    if !plain {
        return Err(unavailable());
    }
    let text = std::fs::read_to_string(&kept).map_err(|_| unavailable())?;
    let (name, value) = text
        .split_once(' ')
        .filter(|(name, value)| {
            teammates_name_valid(name)
                && (*value == TEAMMATES_PIN_PENDING
                    || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())))
        })
        .ok_or_else(unavailable)?;
    if socket.file_name().and_then(|kept| kept.to_str()) != Some(name) {
        return Err(unavailable());
    }
    if value == TEAMMATES_PIN_PENDING {
        return Ok(None);
    }
    let pid = value
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(unavailable)?;
    Ok(Some(pid))
}

/// Records the pid a native lead's teammates' server at one socket first answered with
/// (PLC c, PC-L1), beside the socket's name in the engine's profiles folder, in place of
/// the pending mark its start left. Takes the engine root, the lead, the socket and the
/// pid. Returns Ok, or `terminal_own_server_unavailable`.
#[cfg(unix)]
fn keep_teammates_server_pid(
    engine_root: &Path,
    lead: &AgentId,
    socket: &Path,
    pid: u32,
) -> Result<(), HostError> {
    assert!(pid > 0);
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let name = socket
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| teammates_name_valid(name))
        .ok_or_else(|| failure(OWN_SERVER_UNAVAILABLE))?;
    let text = format!("{name} {pid}");
    assert!(text.len() <= TEAMMATES_PIN_BYTES_MAX);
    keep_record(
        &crate::team::runtime::agent_teammates_server_pid(&engine_root.join("team"), id),
        &text,
    )
}

/// Whether a text is a teammates' socket name as [`renew_teammates_socket`] makes one.
/// Takes the text, read from a file. Returns the answer.
#[cfg(unix)]
fn teammates_name_valid(name: &str) -> bool {
    name.len() == TEAMMATES_NAME_BYTES
        && name
            .strip_prefix(TEAMMATES_SOCKET_PREFIX)
            .and_then(|rest| rest.strip_suffix(TEAMMATES_SOCKET_SUFFIX))
            .is_some_and(|digits| {
                digits.len() == 2 * TEAMMATES_NONCE_BYTES
                    && digits
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
}

/// One named socket in a team agent's short private temp folder. Takes the engine root, the
/// agent and the socket name. Returns the path, or `terminal_agent_invalid` for a name that
/// is not a team agent.
fn lead_temp_socket(engine_root: &Path, lead: &AgentId, name: &str) -> Result<PathBuf, HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let home = crate::team::runtime::agent_home(&engine_root.join("team"), id);
    let socket = crate::sandbox::toolchains::tmp_path(&home).join(name);
    assert!(socket.is_absolute() && socket.as_os_str().len() <= HARNESS_SOCKET_BYTES_MAX);
    Ok(socket)
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
/// The first frame is `Attach`, or `AttachPane` for one pane of an agent's own window, such
/// as a native teammate's, or `AttachSized` for either one whose window is first sized to the
/// viewer, so the screen sent and all output after it fit a viewer that does not reflow;
/// `Input`, `Prompt`, `Resize` and `Detach` follow on the same
/// connection and go to that pane. `Input` bytes, at most `INPUT_BYTES_MAX`, are typed as given and never kept.
/// `Prompt` text, at most `PROMPT_BYTES_MAX`, is the owner's explicit message: it is typed as
/// one submission, and a sub-agent's master later hears a note of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalRequest {
    Attach {
        agent_id: AgentId,
    },
    AttachPane {
        agent_id: AgentId,
        pane: String,
    },
    AttachSized {
        agent_id: AgentId,
        pane: Option<String>,
        cols: u16,
        rows: u16,
    },
    Input {
        bytes: Vec<u8>,
    },
    Prompt {
        text: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
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

/// Types one prompt into an agent's app as one submission: a paste when the app turned
/// bracketed paste on, then Enter once the paste settled. Takes the host, the terminal name and
/// the text. Returns whether it was typed; blank or oversized text types nothing.
pub fn type_prompt(host: &dyn Host, agent: &AgentId, text: &str) -> Result<bool, HostError> {
    let bracketed = host.input_live(agent)?;
    let Some(keys) = prompt_keys(text, bracketed) else {
        return Ok(false);
    };
    for chunk in keys.chunks(INPUT_BYTES_MAX) {
        host.input(agent, chunk)?;
    }
    if bracketed {
        std::thread::sleep(PASTE_SETTLE);
    }
    host.input(agent, b"\r")?;
    Ok(true)
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
