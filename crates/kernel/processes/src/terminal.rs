use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub mod buffer;
#[cfg(target_os = "windows")]
pub mod windows;

pub const AGENT_ID_BYTES_MAX: usize = 64;
pub const INPUT_BYTES_MAX: usize = 4096;
/// The marker that closes a bracketed paste. A program may read typed bytes per read, so
/// no split of them cuts it (RV-DS241-69).
pub const PASTE_CLOSE: &[u8] = b"\x1b[201~";
pub const PROMPT_BYTES_MAX: usize = 4096;
/// The most bytes one owner paste may hold once the engine gathered its `Paste` chunks, the
/// most a native view copies.
pub const PASTE_BYTES_MAX: usize = 1 << 20;
const _: () = assert!(PASTE_BYTES_MAX.is_multiple_of(INPUT_BYTES_MAX));
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
pub const PASTE_SETTLE: Duration = Duration::from_millis(200);

const _: () = assert!(VIEWER_BUFFER_BYTES_MAX >= CHUNK_BYTES_MAX * 16);

/// One program a pane runs, free of harness types: its argument vector, its working folder
/// and its environment, the fields of `InteractiveCommand` the host reads. Built by
/// crazyagent from an `InteractiveCommand`. Inputs: the fields. Output: a value tmux and the
/// Windows host launch unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// The unit key of a terminal that is the agent's own alone, as a host with no shared window
/// sizes it. Takes the agent. Returns the key, which no tmux window key equals, since those
/// start with a socket path.
pub fn agent_unit(agent: &AgentId) -> String {
    format!("agent {agent}")
}

/// A terminal host failure as a stable lowercase code clients show in plain words.
/// `terminal_unavailable` means the host cannot run at all, such as tmux missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// The most bytes a UTF-8 character holds after its first byte.
const CHARACTER_TAIL_BYTES_MAX: usize = 3;
/// The most bytes `input_runs` moves a run's end back to keep an escape sequence, such as a
/// key, a mouse report or a closing paste marker, or a UTF-8 character whole. A sequence that
/// starts further back is cut at the run size.
pub const RUN_CUT_BACK_BYTES_MAX: usize = 64;
const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const _: () = assert!(RUN_CUT_BACK_BYTES_MAX >= PASTE_CLOSE.len());
const _: () = assert!(RUN_CUT_BACK_BYTES_MAX > CHARACTER_TAIL_BYTES_MAX);

/// The most runs `input_runs` cuts `length` bytes into at `run_bytes_max` bytes a run: every
/// run but the last keeps at least `run_bytes_max - RUN_CUT_BACK_BYTES_MAX` bytes. Inputs:
/// the length and the run size, over the back-off. Output: the bound.
pub const fn input_runs_max(length: usize, run_bytes_max: usize) -> usize {
    assert!(run_bytes_max > RUN_CUT_BACK_BYTES_MAX);
    length.div_ceil(run_bytes_max - RUN_CUT_BACK_BYTES_MAX)
}

/// Splits typed bytes into runs of at most `run_bytes_max` bytes, in order. A run ends between
/// tokens, never inside a UTF-8 character or an escape sequence (a key, a mouse report, a
/// reply or a closing paste marker), so a program that reads per read never sees half of one
/// and takes a lone ESC for the Escape key (RV-DS241-69, #359). Inputs: the bytes and the run
/// size, over the back-off. Output: the runs, none for no bytes.
pub fn input_runs(bytes: &[u8], run_bytes_max: usize) -> Vec<&[u8]> {
    assert!(run_bytes_max > RUN_CUT_BACK_BYTES_MAX);
    let mut runs = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let limit = start + run_bytes_max;
        let end = if limit >= bytes.len() {
            bytes.len()
        } else {
            run_end(&bytes[..=limit], start)
        };
        assert!(end > start);
        assert!(end <= limit);
        runs.push(&bytes[start..end]);
        start = end;
    }
    assert!(runs.iter().all(|run| run.len() <= run_bytes_max));
    assert!(runs.len() <= input_runs_max(bytes.len(), run_bytes_max));
    assert_eq!(runs.concat(), bytes);
    runs
}

/// Where a run that starts at `start` ends, given the bytes up to one past its size: at the
/// last token end at most `RUN_CUT_BACK_BYTES_MAX` bytes back, else at the size moved back to
/// a UTF-8 character's first byte. Inputs: that window and the start, where tokens are read
/// from. Output: the end, past the start and at most the size.
fn run_end(window: &[u8], start: usize) -> usize {
    let limit = window.len() - 1;
    assert!(limit > start + RUN_CUT_BACK_BYTES_MAX);
    let mut boundary = start;
    while boundary < limit {
        let next = token_end(window, boundary);
        if next > limit {
            break;
        }
        boundary = next;
    }
    let end = if boundary + RUN_CUT_BACK_BYTES_MAX >= limit {
        boundary
    } else {
        (limit - CHARACTER_TAIL_BYTES_MAX..=limit)
            .rev()
            .find(|&at| window[at] & 0xc0 != 0x80)
            .unwrap_or(limit)
    };
    assert!(end > start);
    assert!(end <= limit);
    end
}

/// Where the typed token that starts at `at` ends: one UTF-8 character, or one escape
/// sequence (CSI, an X10 mouse report, SS3 and one character, OSC or DCS up to BEL or ESC \,
/// or ESC and one character). A sequence the bytes stop inside runs to their end. Inputs:
/// the bytes and a position in them. Output: the end, past `at`.
fn token_end(bytes: &[u8], at: usize) -> usize {
    assert!(at < bytes.len());
    let end = match (bytes[at], bytes.get(at + 1)) {
        (ESC, Some(b'[')) => csi_end(bytes, at + 2),
        (ESC, Some(b'O')) if at + 2 < bytes.len() => character_end(bytes, at + 2),
        (ESC, Some(b'O')) => at + 2,
        (ESC, Some(b']' | b'P')) => string_end(bytes, at + 2),
        (ESC, Some(&ESC) | None) => at + 1,
        (ESC, Some(_)) => character_end(bytes, at + 1),
        _ => character_end(bytes, at),
    };
    assert!(end > at);
    assert!(end <= bytes.len());
    end
}

/// The end of a CSI whose parameters start at `from`: past its final byte, three bytes on
/// for an X10 mouse report (`ESC [ M` and three raw bytes, as xterm.js sends them, never
/// UTF-8), or before a byte no CSI holds. Inputs: the bytes and the position after `ESC [`.
/// Output: the end.
fn csi_end(bytes: &[u8], from: usize) -> usize {
    let Some(tail) = bytes[from..]
        .iter()
        .position(|byte| !(0x20..=0x3f).contains(byte))
    else {
        return bytes.len();
    };
    let last = from + tail;
    match bytes[last] {
        b'M' if tail == 0 => (last + 4).min(bytes.len()),
        0x40..=0x7e => last + 1,
        _ => last,
    }
}

/// The end of an OSC or DCS string whose body starts at `from`: past its BEL or its ESC \,
/// or before any other ESC, which ends the string and starts the next sequence. Inputs: the
/// bytes and the position after `ESC ]` or `ESC P`. Output: the end.
fn string_end(bytes: &[u8], from: usize) -> usize {
    let Some(stop) = bytes[from..]
        .iter()
        .position(|&byte| byte == BEL || byte == ESC)
    else {
        return bytes.len();
    };
    let at = from + stop;
    match (bytes[at], bytes.get(at + 1)) {
        (BEL, _) | (_, None) => at + 1,
        (_, Some(b'\\')) => at + 2,
        _ => at,
    }
}

/// The end of the UTF-8 character that starts at `at`: past its continuation bytes, at most
/// `CHARACTER_TAIL_BYTES_MAX` of them; a byte that is not UTF-8 ends alone. Inputs: the bytes
/// and a position in them. Output: the end.
fn character_end(bytes: &[u8], at: usize) -> usize {
    let tail = bytes[at + 1..]
        .iter()
        .take(CHARACTER_TAIL_BYTES_MAX)
        .take_while(|&&byte| byte & 0xc0 == 0x80)
        .count();
    at + 1 + tail
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

const VIEW_RESET: &[u8] =
    b"\x1b[?1049l\x1b[?2004l\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l";

/// One viewer's live view of an agent's terminal: its size, the screen at attach, then output.
/// `history`, written first, fills a fresh view's scrollback (empty when the host keeps none);
/// `screen` then redraws the whole screen in a fresh `cols` by `rows` terminal. `next` gives
/// later output in order, `Chunk::Behind` once the viewer lost output (the viewer then attaches
/// again and writes that stream's `redraw`), and none once the terminal ends.
#[derive(Debug)]
pub struct TerminalStream {
    pub agent: AgentId,
    pub cols: u16,
    pub rows: u16,
    pub history: Vec<u8>,
    pub screen: Vec<u8>,
    pub chunks: buffer::Chunks,
    #[cfg(unix)]
    pub at: Option<PaneAt>,
}

/// The pane a view was attached to and the serial of the host connection that found it, so
/// the view's keys can go straight to that pane while that same connection still serves it.
/// Fields: the pane id and the connection serial, unique within the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneAt {
    pub pane: String,
    pub serial: u64,
}

/// Types a view's keys: straight into the pane its stream was attached to when the host can
/// still use it, else by the agent's name as `input` does. Takes the host the view was
/// attached through, the view's pane, the agent and at most `INPUT_BYTES_MAX` bytes.
/// Returns as `input`.
#[cfg(unix)]
pub fn type_into(
    host: &dyn Host,
    at: Option<&PaneAt>,
    agent: &AgentId,
    bytes: &[u8],
) -> Result<(), HostError> {
    if let Some(at) = at
        && host.input_at(agent, at, bytes)?
    {
        return Ok(());
    }
    host.input(agent, bytes)
}

impl TerminalStream {
    /// Waits for the next read. Takes the stream. Returns output of at most
    /// `CHUNK_BYTES_MAX` bytes, `Chunk::Behind` once output was lost, or none once the
    /// terminal ended.
    pub async fn next(&mut self) -> Option<buffer::Chunk> {
        self.chunks.recv().await
    }

    /// Waits for the next read outside an async runtime. Takes the stream. Returns as `next`.
    pub fn next_blocking(&mut self) -> Option<buffer::Chunk> {
        self.chunks.blocking_recv()
    }

    /// The bytes that bring a view that was already drawing this terminal back to its screen
    /// now, after it fell behind: every mode a screen can turn on is turned off, then `screen`
    /// redraws the whole screen with the modes the agent uses. Takes the stream. Returns them.
    pub fn redraw(&self) -> Vec<u8> {
        let mut bytes = VIEW_RESET.to_vec();
        bytes.extend_from_slice(&self.screen);
        bytes
    }
}

/// Opens a fresh viewer in place of one that read `Chunk::Behind`, on a blocking thread, so
/// the view goes on from the screen as it is now with nothing lost or repeated after it.
/// Takes the host the old viewer came from and the terminal name. Returns the new stream,
/// whose `redraw` the view writes first, or the host's code, such as `terminal_agent_unknown`.
pub async fn attach_again(
    host: Arc<dyn Host>,
    agent: AgentId,
) -> Result<TerminalStream, HostError> {
    tokio::task::spawn_blocking(move || host.attach(&agent))
        .await
        .map_err(|_| failure("terminal_unavailable"))?
}

/// Runs every agent's interactive app in a real terminal that outlives the engine and its viewers.
/// macOS and Linux use the engine's private tmux server; Windows has its own host behind this trait.
/// Every method fails with `terminal_unavailable` when the host cannot run on this machine.
pub trait Host: Send + Sync {
    /// Starts one agent's command in a new terminal named after the agent.
    /// Takes the name and the sandboxed command; fails with `terminal_agent_running` for a live name.
    /// Returns where it runs.
    fn spawn(&self, agent: &AgentId, command: &PaneCommand) -> Result<PaneRef, HostError>;

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

    /// Types bytes straight into the pane a view of this agent was attached to, with no
    /// lookup, while the connection that found that pane still serves it. Takes the name,
    /// the pane from the view's stream and at most `INPUT_BYTES_MAX` bytes. Returns true
    /// once typed, false when this host cannot use that pane and the caller types by name.
    fn input_at(&self, _agent: &AgentId, _at: &PaneAt, _bytes: &[u8]) -> Result<bool, HostError> {
        Ok(false)
    }

    /// The pane an input-only view of an agent stays on: the agent's own pane now, the one
    /// `attach` would view, and the serial of the connection that found it. Takes the name.
    /// Returns the pane, `terminal_agent_unknown` once the terminal is gone, or
    /// `terminal_unavailable` for a host with no pane ids, whose views type by name.
    fn own_pane_at(&self, _agent: &AgentId) -> Result<PaneAt, HostError> {
        Err(failure("terminal_unavailable"))
    }

    /// Marks the pane of an agent's window whose first process is `pid` as the agent's app
    /// pane, the one its prompts go to, for a pane an older engine started without that mark.
    /// Takes the name and the pid its pane record kept. Returns Ok once marked, at once on a
    /// host with no pane ids, or `APP_PANE_UNKNOWN` when no live pane has that pid.
    fn tag_app(&self, _agent: &AgentId, _pid: u32) -> Result<(), HostError> {
        Ok(())
    }

    /// The pane an agent's prompts go to: its app's own pane, never a teammate split off its
    /// window, and the serial of the connection that found it. Takes the name. Returns the
    /// pane, `APP_PANE_UNKNOWN` when no pane is known to be the app's, or `PINS_UNSUPPORTED`
    /// for a host with no pane ids, whose prompts type by name.
    fn app_pane_at(&self, _agent: &AgentId) -> Result<PaneAt, HostError> {
        Err(failure(PINS_UNSUPPORTED))
    }

    /// Pastes keys straight into the pane a view stays on, as `paste` does by name, while the
    /// connection that found that pane still serves it. Takes the name, the pane and 1 to
    /// `PASTE_BYTES_MAX` keys. Returns true once pasted, false when this host cannot use it.
    fn paste_at(&self, _agent: &AgentId, _at: &PaneAt, _keys: &[u8]) -> Result<bool, HostError> {
        Ok(false)
    }

    /// Sizes the window of the pane a view stays on, as `resize` does by name, while the
    /// connection that found that pane still serves it. Takes the name, the pane and the
    /// size. Returns true once sized, false when this host cannot use that pane.
    fn resize_at(
        &self,
        _agent: &AgentId,
        _at: &PaneAt,
        _size: (u16, u16),
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    /// Names what a resize of the agent sizes, one key for every view whose resize sizes the
    /// same thing, so the engine counts them together: by default the agent's own terminal.
    /// Takes the name. Returns the unit's key.
    fn unit(&self, agent: &AgentId) -> Result<String, HostError> {
        Ok(agent_unit(agent))
    }

    /// Tells whether the app in an agent terminal reads keys itself yet: interactive apps
    /// turn on bracketed paste once their own input is live, and keys typed before that
    /// go through the terminal's line editing, which turns Enter into a newline.
    /// Takes the name. Returns true once it is on, and always for a host that cannot tell.
    fn input_live(&self, _agent: &AgentId) -> Result<bool, HostError> {
        Ok(true)
    }

    /// Tells whether bracketed paste is on in the pane a view or a prompt stays on, as
    /// `input_live` tells it by name, while the connection that found that pane still serves
    /// it. Takes the name and the pane. Returns the answer, none when this host cannot use
    /// that pane, or the answer by name on a host with no pane ids.
    fn input_live_at(&self, agent: &AgentId, _at: &PaneAt) -> Result<Option<bool>, HostError> {
        self.input_live(agent).map(Some)
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

    /// Pastes the owner's keys into the agent's own pane as one paste, bracketed only when
    /// its app turned bracketed paste on. Takes the name and keys from `paste_keys`, 1 to
    /// `PASTE_BYTES_MAX` bytes. Returns once the host took them all, or
    /// `terminal_paste_unsupported` from a host that cannot paste.
    fn paste(&self, _agent: &AgentId, _keys: &[u8]) -> Result<(), HostError> {
        Err(failure("terminal_paste_unsupported"))
    }

    /// Pastes the owner's keys into one pane as `paste` does into the agent's own pane.
    /// Takes the name, the pane id and the keys. Returns as `paste`.
    fn paste_pane(&self, _agent: &AgentId, _pane: &str, _keys: &[u8]) -> Result<(), HostError> {
        Err(failure("terminal_paste_unsupported"))
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
        _command: &PaneCommand,
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

    /// Whether this host runs an agent's pane without a keeper, as a native lead's own server
    /// does that runs inside the lead's sandbox with no server keeper, such as one an earlier
    /// engine started: no warden then ends what the pane's agent started, so its stop is
    /// judged by the processes still found. Takes the name. Returns the answer; false for a
    /// host that runs each pane as given or knows the keeper of the agent's server.
    fn unkept(&self, _agent: &AgentId) -> bool {
        false
    }

    /// Ends an agent's own server through the keeper this host started it under, as a native
    /// lead's own server runs (KEEPER (b)): the keeper's warden kills every process in the
    /// agent's sandbox, the server and its panes included, and the keeper exits only once the
    /// warden confirmed that. Takes the name. Returns whether the keeper confirmed in time,
    /// or none when the host knows no keeper for the agent, as for a server an earlier engine
    /// started.
    fn end_kept_server(&self, _agent: &AgentId) -> Option<bool> {
        None
    }

    /// The keepers of a native lead's own and teammates' servers this host started, each as
    /// its process id and whether it keeps the teammates' server, so a quitting engine can
    /// freeze both (DEC-158). Takes the name. Returns them, own first; none for a host that
    /// started no server for the agent.
    fn server_keepers(&self, _agent: &AgentId) -> Vec<(u32, bool)> {
        Vec::new()
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

/// Whether a host's code says the terminal a view or a call asked for ended for good: no
/// server holds the agent (`terminal_agent_unknown`), or the teammates' server a native
/// lead's team panes were on ended ([`TEAMMATES_SERVER_ENDED`]), as once both of that lead's
/// servers ended. Takes the code. Returns the answer.
pub fn ended_code(code: &str) -> bool {
    matches!(code, "terminal_agent_unknown" | TEAMMATES_SERVER_ENDED)
}

/// What a host with no pane ids answers for an agent's app pane, so its prompts type by name.
/// No tmux host answers it, so a failed connect fails the prompt instead (#686 r1 sec F1).
pub const PINS_UNSUPPORTED: &str = "terminal_pins_unsupported";

/// The refusal for a prompt to an agent whose app pane is not known while its window lives: a
/// split window with no tag, an app pane gone beside a teammate, or a request's pin gone
/// stale. Not an ended terminal, so a view stays and coord holds the message (#686 r1 F2).
pub const APP_PANE_UNKNOWN: &str = "terminal_app_pane_unknown";

/// The refusal for a native lead's teammates' socket answered by a server under another pid
/// than the one the engine first reached there (PLC c).
pub const TEAMMATES_SERVER_REPLACED: &str = "terminal_teammates_server_replaced";

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
    fn spawn(&self, _agent: &AgentId, _command: &PaneCommand) -> Result<PaneRef, HostError> {
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
    fn input_at(&self, agent: &AgentId, at: &PaneAt, bytes: &[u8]) -> Result<bool, HostError> {
        if at.pane != self.pane {
            return Ok(false);
        }
        self.host.input_at(agent, at, bytes)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.host.input_live_pane(agent, &self.pane)
    }

    fn input_live_at(&self, agent: &AgentId, at: &PaneAt) -> Result<Option<bool>, HostError> {
        if at.pane != self.pane {
            return Ok(None);
        }
        self.host.input_live_at(agent, at)
    }

    fn paste(&self, agent: &AgentId, keys: &[u8]) -> Result<(), HostError> {
        self.host.paste_pane(agent, &self.pane, keys)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        self.host.quiet(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.host.resize(agent, cols, rows)
    }

    fn unit(&self, agent: &AgentId) -> Result<String, HostError> {
        self.host.unit(agent)
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
/// whether team agents keep running (the owner's quit settings). Returns the
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

#[cfg(test)]
mod tests;
