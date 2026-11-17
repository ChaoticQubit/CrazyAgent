use super::accounts::Handover;
use super::drivers::acp::Acp;
use super::drivers::{self, Piped};
use super::interactive::KEY_FILE;
use super::leftovers::{Follower, LINEAGE_VARIABLE, LineageToken, Unstopped};
use super::terminal_session::{KeyFile, write_key_file};
use crate::adapters::claude::{ClaudeProtocol, State as ClaudeState};
use crate::adapters::codex::{CodexProtocol, Event as CodexEvent, State as CodexState, TurnStatus};
use crate::adapters::events::{EventKind, RateLimitStatus, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::credentials::SecretString;
use crate::launch_context::{AgentContext, EngineServer, Installed};
use crate::model_gateway::relay::{Relay, is_placeholder};
use crate::protocol::{CredentialRef, RequestId, SessionId};
use crate::sandbox::toolchains::{self, ToolHomes};
use crate::usage::capture::Tokens;
#[cfg(unix)]
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::Serialize;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

pub(super) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const LINE_BYTES_MAX: usize = crate::adapters::LINE_BYTES_MAX;
/// The longest prompt a launch sends: half of one outbound Codex frame, which leaves room
/// for the JSON-RPC envelope around it.
pub(crate) const PROMPT_BYTES_MAX: usize = crate::adapters::codex::FRAME_BYTES_MAX / 2;
/// The most input bytes a session keeps for a harness that has not taken them yet: room for
/// the largest prompt frame with every byte escaped to six, its envelope and an interrupt, so
/// only a harness that stopped reading reaches it (T20 sec r1 F1).
const INPUT_PENDING_MAX: usize = 8 * PROMPT_BYTES_MAX;
const _: () = assert!(6 * PROMPT_BYTES_MAX + 8192 <= INPUT_PENDING_MAX);
/// How long `next_event` waits on output between tries to hand a harness the input it has
/// not taken yet.
const INPUT_RETRY: Duration = Duration::from_millis(20);
pub(super) const TOOLS_OFF_PATH: &str = "/usr/bin:/bin";
pub(super) const TOOLS_ON_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
pub(super) const LOCAL_PLACEHOLDER_KEY: &str = crate::model_gateway::relay::LOCAL_PLACEHOLDER;
const CODEX_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const CLAUDE_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DURABLE_INPUT: &str = "input";
pub const DURABLE_OUTPUT: &str = "output.log";
/// The most bytes a durable harness output log holds before the engine empties it: the
/// session's reader once it has read all of it, the watcher of a reattached agent, whose log
/// nothing reads, at once. The agent runs on either way (owner 11-11).
pub const DURABLE_OUTPUT_KEPT_MAX: u64 = 64 * 1024 * 1024;
#[cfg(unix)]
const MKFIFO: &str = "/usr/bin/mkfifo";
const TAIL_POLL: Duration = Duration::from_millis(20);
#[cfg(unix)]
const GROUP_EXIT_WAIT: Duration = Duration::from_secs(10);
/// The Codex `-c` settings every tools-off launch passes: a read-only sandbox,
/// no shell or exec, patch, image, sub-agent, code mode, app, plugin, goal or
/// web search tool and no MCP server.
pub(crate) const CODEX_TOOLS_OFF: [&str; 13] = [
    "sandbox_mode=\"read-only\"",
    "approval_policy=\"never\"",
    "features.shell_tool=false",
    "features.apply_patch_freeform=false",
    "features.unified_exec=false",
    "features.view_image=false",
    "features.multi_agent=false",
    "features.code_mode=false",
    "features.apps=false",
    "features.plugins=false",
    "features.goals=false",
    "web_search=\"disabled\"",
    "mcp_servers={}",
];
/// The Codex 0.164 setting that removes `request_user_input`, the one tool a tools-off
/// research agent or pane agent is still offered after [`CODEX_TOOLS_OFF`].
pub(crate) const CODEX_NO_USER_INPUT: &str = "tools.experimental_request_user_input.enabled=false";
const CODEX_TOOLS_ON: [&str; 8] = [
    "sandbox_mode=\"danger-full-access\"",
    "approval_policy=\"never\"",
    "features.apps=false",
    "features.plugins=false",
    "features.goals=false",
    "features.multi_agent=false",
    CODEX_MULTI_AGENT_V2_OFF,
    "mcp_servers={}",
];
/// The tools-on Codex setting that keeps multi-agent v2 off; a native lead's launch leaves it
/// out so the v2 table its `config.toml` holds turns Codex's own sub-agents on (PA-7).
const CODEX_MULTI_AGENT_V2_OFF: &str = "features.multi_agent_v2=false";

type Invocation = (Vec<OsString>, Vec<(&'static str, OsString)>);

pub(super) const CLAUDE_NO_MCP: [&str; 2] = ["--mcp-config", "{\"mcpServers\":{}}"];
pub(crate) const CLAUDE_DENY_FLAG: &str = "--disallowedTools";
/// Claude Code's own sub-agent tools, denied to every tools-on Claude agent the engine starts,
/// master and team alike, so sub-agents start only through the engine. Claude Code 2.1.309
/// names them `Agent` (offered as `Task` in print mode, its alias), `Workflow` and
/// `TeamCreate`; denying a name also denies its aliases. Every other default tool stays.
pub(crate) const CLAUDE_BUILT_IN_AGENTS: [&str; 3] = ["Agent", "Workflow", "TeamCreate"];
/// The Claude Code team tools a native-team lead keeps (DEC-104 Q10): `TeamCreate` starts
/// its agent team and `Agent` starts each teammate. `Workflow` stays denied, and every
/// teammate is denied both again.
pub(crate) const CLAUDE_NATIVE_TEAM_TOOLS: [&str; 2] = ["Agent", "TeamCreate"];
/// Turns off Claude Code's background Bash commands and background agents for a tools-on
/// team agent. In Claude Code 2.1.309 a finished background task starts another turn after
/// the turn's result, which the engine would read as the next task's reply.
const CLAUDE_BACKGROUND_TASKS_OFF: (&str, &str) = ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
/// Gives a tools-on Claude Code agent 60 s, not its own 30 s, to connect its MCP servers.
/// context-mode from the engine's shared install answers in about 1 s; the margin covers a
/// machine busy with builds, and a server that never answers delays the start by 60 s only.
pub(super) const CLAUDE_MCP_CONNECT_LIMIT: (&str, &str) = ("MCP_TIMEOUT", "60000");
/// Turns Claude Code's auto-memory off for every agent launch, beside `autoMemoryEnabled`
/// false in its settings (RV-DS241-205): the home reset keeps `projects` for transcripts and
/// clears each `projects/<key>/memory` only at the next reset (B20), so a `MEMORY.md` one
/// session wrote would otherwise load into every later session of that launch, teammates
/// included, as standing instructions.
pub(crate) const CLAUDE_AUTO_MEMORY_OFF: (&str, &str) = ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
const CLAUDE_SETTINGS: &str = "{\"attribution\":{\"commit\":\"\",\"pr\":\"\"},\"autoMemoryEnabled\":false,\"disableAllHooks\":true,\"includeCoAuthoredBy\":false}";
const CLAUDE_COMMON: [&str; 12] = [
    "--print",
    "--verbose",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--strict-mcp-config",
    "--setting-sources",
    "",
    "--settings",
    CLAUDE_SETTINGS,
    CLAUDE_NO_TRANSCRIPT,
];
const CLAUDE_NO_TRANSCRIPT: &str = "--no-session-persistence";
/// The system prompt of every tools-off Claude Code launch, in place of Claude Code's own
/// agent prompt. That prompt describes Bash and other tools such a launch does not offer;
/// under it a Claude Code 2.1.309 mapping run wrote tool calls as plain text until its
/// output limit and kept going, so onboarding hung for its whole step time.
pub(crate) const CLAUDE_NO_TOOLS_PROMPT: &str = "You are Claude, answering inside CrazyAgent with no tools. You cannot run commands, read or write files, browse or call any tool, and no tool results will ever come back. Never write tool calls, function call markup or commands for yourself to run. Answer directly in plain text from the conversation and the text you are given.";
/// Claude Code's arguments for a launch with tools off: no tools, no slash commands and
/// [`CLAUDE_NO_TOOLS_PROMPT`] as the whole system prompt.
pub(crate) const CLAUDE_TOOLS_OFF: [&str; 5] = [
    "--tools",
    "",
    "--disable-slash-commands",
    "--system-prompt",
    CLAUDE_NO_TOOLS_PROMPT,
];

/// Holds an API key or token in memory for one launch; it never appears in Debug,
/// serialization, arguments or reports, is handed to the child only as an environment value,
/// and is zeroed when it is dropped.
pub struct Secret(String);

impl Secret {
    /// Wraps a key after bounds checks; a refused key is zeroed. Input: the key text. Output:
    /// the secret, or `launch_secret_invalid` when it is empty, oversized or contains
    /// whitespace or controls.
    pub fn new(value: String) -> Result<Self, LaunchError> {
        if value.is_empty()
            || value.len() > 4096
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            crate::credentials::wipe(&mut value.into_bytes());
            return Err(failure("launch_secret_invalid"));
        }
        Ok(Self(value))
    }

    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }

    pub(super) fn reveal(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        crate::credentials::wipe(&mut std::mem::take(&mut self.0).into_bytes());
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

/// The account a launch runs as. Every variant names the account's own managed home
/// directory; the owner's real `~/.codex` and `~/.claude` are always refused.
#[derive(Debug)]
pub enum AccountBinding {
    Subscription {
        home: PathBuf,
    },
    ApiKey {
        home: PathBuf,
        key: Secret,
        base_url: Option<String>,
    },
    Url {
        home: PathBuf,
        base_url: String,
        key: Option<Secret>,
    },
}

impl AccountBinding {
    /// Returns the account's managed home directory.
    pub fn home(&self) -> &Path {
        match self {
            Self::Subscription { home } | Self::ApiKey { home, .. } | Self::Url { home, .. } => {
                home
            }
        }
    }

    pub(super) fn base_url(&self) -> Option<&str> {
        match self {
            Self::Subscription { .. } => None,
            Self::ApiKey { base_url, .. } => base_url.as_deref(),
            Self::Url { base_url, .. } => Some(base_url),
        }
    }

    pub(super) fn key(&self) -> Option<&Secret> {
        match self {
            Self::Subscription { .. } => None,
            Self::ApiKey { key, .. } => Some(key),
            Self::Url { key, .. } => key.as_ref(),
        }
    }
}

/// The longest one launched harness process may run: 24 hours.
pub const RUN_TIME_MAX: Duration = Duration::from_secs(86_400);

/// Bounds for one launched harness process.
/// `run_time` caps the whole session and `stop_grace` the wait between TERM and KILL.
/// `stale_turn` is how long an open turn of a session that follows every turn may get no
/// new byte in the session record, on a pane whose app waits, before it counts as no turn.
/// No count of output bytes or events ends a session (owner 11-11): memory is bounded per
/// line and per read, and a line past the bound is passed over.
#[derive(Clone, Copy, Debug)]
pub struct LaunchLimits {
    pub run_time: Duration,
    pub stop_grace: Duration,
    pub stale_turn: Duration,
    pub provider_retry: crate::provider_retry::ProviderRetry,
}

impl Default for LaunchLimits {
    fn default() -> Self {
        Self {
            run_time: Duration::from_secs(600),
            stop_grace: Duration::from_secs(2),
            stale_turn: Duration::from_secs(600),
            provider_retry: crate::provider_retry::ProviderRetry::HarnessDefault,
        }
    }
}

/// Everything needed to start one harness: catalog id, executable, working directory,
/// account, optional model, tools switch, the project sandbox profile, whether the project
/// gave its agents Full access, the owner's git identity for the agent's own commits and
/// limits. Every launch is refused unless a sandbox profile is given, or no profile with
/// `unconfined` set for a Full access project (DEC-102).
#[derive(Debug)]
pub struct LaunchRequest {
    pub harness: String,
    pub executable: PathBuf,
    pub working_dir: PathBuf,
    pub account: AccountBinding,
    pub model: Option<String>,
    pub tools: bool,
    pub sandbox_profile: Option<PathBuf>,
    pub unconfined: bool,
    pub git_identity: Option<GitIdentity>,
    pub limits: LaunchLimits,
}

const GIT_IDENTITY_BYTES_MAX: usize = 256;

/// The git author and committer an agent's own commits carry: the owner's `user.name`
/// and `user.email`, each non-empty, at most 256 bytes and free of control characters
/// and angle brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    name: String,
    email: String,
}

impl GitIdentity {
    /// Checks an identity. Inputs: the name and email. Output: the identity, or
    /// `launch_git_identity_invalid`.
    pub fn new(name: String, email: String) -> Result<Self, LaunchError> {
        let valid = |value: &str| {
            !value.trim().is_empty()
                && value.len() <= GIT_IDENTITY_BYTES_MAX
                && !value
                    .chars()
                    .any(|c| c.is_control() || c == '<' || c == '>')
        };
        if !valid(&name) || !valid(&email) {
            return Err(failure("launch_git_identity_invalid"));
        }
        Ok(Self { name, email })
    }

    /// The variables that make git author and commit as this identity.
    pub(crate) fn variables(&self) -> [(&'static str, &str); 4] {
        [
            ("GIT_AUTHOR_NAME", &self.name),
            ("GIT_AUTHOR_EMAIL", &self.email),
            ("GIT_COMMITTER_NAME", &self.name),
            ("GIT_COMMITTER_EMAIL", &self.email),
        ]
    }
}

/// One event in the stream every launch adapter produces, whatever the harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchEvent {
    Ready { native_session: Option<String> },
    Text { text: String },
    ToolUse { name: String },
    ApprovalDenied,
    Interrupted,
    TurnFinished { success: bool },
    Usage { tokens: Tokens },
    TurnError { rate_limited: bool },
    Exited { code: Option<i32> },
    LateOutput { lines: u64 },
}

/// Tells whether a provider error message says the account hit a rate or usage limit.
/// Takes the provider's error text, which is never stored or shown.
/// Returns true for rate limit, usage limit, quota and HTTP 429 wording, and for the
/// limit lines Claude Code writes itself.
pub fn rate_limited(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "rate limit",
        "rate_limit",
        "usage limit",
        "usage_limit",
        "quota",
        "429",
        "too many requests",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || claude_limit_line(message.trim_start())
}

/// Tells whether a line is one of Claude Code's own limit lines, by the check Claude
/// Code 2.1.309 ships (`Bii`): one of its limit prefixes, or a model family, an optional
/// name of 1 to 40 characters without a middle dot or line break, then
/// ` requires usage credits.`. Takes the line. Returns true for a limit line.
fn claude_limit_line(line: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "You've hit your",
        "You've reached your",
        "You're out of usage credits",
        "Your org is out of usage \u{b7} add funds to continue",
        "Your org is out of usage \u{b7} contact your admin",
        "Your seat type doesn't include usage credits",
        "Your seat type doesn't include usage",
        "Your usage allocation has been disabled by your admin",
        "Your group's usage limit is set to $0",
        "Fable 5 requires usage credits",
        "You're out of extra usage",
        "Your seat type doesn't include extra usage",
    ];
    const FAMILIES: [&str; 5] = ["Fable", "Haiku", "Mythos", "Opus", "Sonnet"];
    const CREDITS: &str = " requires usage credits.";
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
        || FAMILIES.iter().any(|family| {
            line.strip_prefix(family).is_some_and(|rest| {
                rest.starts_with(CREDITS)
                    || rest
                        .strip_prefix(' ')
                        .and_then(|tail| tail.find(CREDITS).map(|end| &tail[..end]))
                        .is_some_and(|name| {
                            (1..=40).contains(&name.chars().count())
                                && !name.contains(['\u{b7}', '\n'])
                        })
            })
        })
}

/// A plain failure code from a launch adapter. It never carries secrets or raw output; a
/// `launch_secret_in_command` refusal also names the MCP server and secret, never the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchError(pub String, pub Option<Box<SecretInCommand>>);

/// The owner-given names of the MCP server and secret whose value a refused launch command
/// would have held. Names are validated when saved, so they are plain text to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretInCommand {
    pub server: String,
    pub secret: String,
}

pub(super) fn failure(code: &str) -> LaunchError {
    LaunchError(code.into(), None)
}

/// Sends SIGKILL to every process of an agent's process group. Takes the group id, which
/// names the group's leader and is above 1; an id that names no process does nothing.
#[cfg(unix)]
pub(crate) fn kill_group(group: u32) {
    let Ok(raw) = i32::try_from(group) else {
        return;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return;
    };
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// How long a group its holder saw end stays refused to every later claim: past
/// `GROUP_FRESH` and the keeper's wait, so a session that saw its pane alive just before
/// another path ended the group finds the number refused and only watches it (CB-2 r1 F1).
const ENDED_REFUSED: Duration = Duration::from_secs(10);

/// The process groups some path is ending now, with no time, and those a holder saw end,
/// with the time it saw that, so a pane's stop and the terminal host's close never both
/// signal one group (CB-2 one signal) and no later path claims a number that may lead a
/// new group by then (CB-2 r1 F1).
static ENDING: std::sync::Mutex<std::collections::BTreeMap<u32, Option<Instant>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// One path's hold on ending a process group: while it lives every other path leaves the
/// group alone. Dropping it frees the number, or, once its holder saw the group answer
/// ESRCH, keeps it refused for `ENDED_REFUSED`, as it may lead a new group by then.
pub(crate) struct Ending {
    group: u32,
    gone: bool,
}

impl Ending {
    /// Takes the hold on a process group. Input: the group id, a pid above 1. Output: the
    /// hold, or none while another path holds that group or a holder saw it end within
    /// `ENDED_REFUSED`.
    pub(crate) fn claim(group: u32) -> Option<Self> {
        assert!(group > 1 && i32::try_from(group).is_ok());
        let mut ending = ENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ending.retain(|_, ended| ended.is_none_or(|at| at.elapsed() < ENDED_REFUSED));
        if ending.contains_key(&group) {
            return None;
        }
        ending.insert(group, None);
        Some(Self { group, gone: false })
    }

    /// The group this hold names. Output: its id as a pid.
    #[cfg(unix)]
    pub(crate) fn pid(&self) -> rustix::process::Pid {
        i32::try_from(self.group)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .expect("a held group id is a pid above 1")
    }

    /// Records that the group answered ESRCH, so dropping this hold keeps its number
    /// refused for `ENDED_REFUSED` instead of freeing it.
    #[cfg(unix)]
    pub(crate) fn gone(&mut self) {
        self.gone = true;
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        let mut ending = ENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.gone {
            ending.insert(self.group, Some(Instant::now()));
        } else {
            ending.remove(&self.group);
        }
    }
}

/// Ends a pane's process group once its pane is gone, from the one path that holds it
/// (CB-2 one signal): KILL every 10 ms while the group answers, for at most
/// `GROUP_EXIT_WAIT`, testing it before each KILL, so a group that answered ESRCH is never
/// signalled again, as its number may lead a new group by then. macOS answers EPERM for a
/// group whose last member it just reaped, so only ESRCH ends it. A group another path
/// holds or a holder just saw end, as the terminal host's close of that pane, is only
/// watched and never claimed here (CB-2 r1 F1). Inputs: the group id and this path's hold,
/// none to only watch. Output: whether the group is gone.
#[cfg(unix)]
pub(crate) fn end_pane_group(group: u32, held: Option<Ending>) -> bool {
    let Some(pid) = i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return true;
    };
    end_pane_group_with(
        held,
        GROUP_EXIT_WAIT,
        || rustix::process::test_kill_process_group(pid) != Err(rustix::io::Errno::SRCH),
        || {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        },
    )
}

/// Ends a group as `end_pane_group` describes, through `left`, which tells whether the
/// group still answers, and `kill`, which sends it KILL, called only with a hold. The hold
/// is dropped on return, marked gone when the group answered ESRCH. Inputs: the hold, the
/// longest wait and both calls. Output: whether the group is gone.
#[cfg(unix)]
fn end_pane_group_with(
    mut held: Option<Ending>,
    wait: Duration,
    mut left: impl FnMut() -> bool,
    mut kill: impl FnMut(),
) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if !left() {
            if let Some(held) = held.as_mut() {
                held.gone();
            }
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if held.is_some() {
            kill();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until no process of a killed agent's group is left, killing again
/// any member that is still there. Takes the group id; returns once the group
/// is empty or after `GROUP_EXIT_WAIT`, so a run never reports an agent as
/// stopped while one of its processes still lingers unnoticed. Output: whether
/// the group is empty.
#[cfg(unix)]
pub(crate) fn await_group_exit(group: u32) -> bool {
    let Some(pid) = i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return true;
    };
    let started = Instant::now();
    while rustix::process::test_kill_process_group(pid).is_ok()
        && started.elapsed() < GROUP_EXIT_WAIT
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        std::thread::sleep(Duration::from_millis(10));
    }
    !group_alive(group)
}

/// Tells whether a process of a group is still there, without signalling it. Takes the
/// group id. Output: false for an id that names no process.
#[cfg(unix)]
pub(crate) fn group_alive(group: u32) -> bool {
    i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

/// Routes a launch that carries a real provider key through an engine-owned key relay on
/// 127.0.0.1: the relay keeps the key in engine memory and the returned request names the
/// relay's base URL and a random placeholder token instead, so the key never reaches the
/// agent's environment, arguments or files. Keyless, subscription and already routed
/// launches come back unchanged. Input: the request. Output: the request to launch and
/// the relay, which must live as long as the agent; `launch_key_relay_unsupported` for a
/// harness whose model protocol the relay does not carry (only Codex and Claude Code).
pub fn route(mut request: LaunchRequest) -> Result<(LaunchRequest, Option<Relay>), LaunchError> {
    let Some(key) = request
        .account
        .key()
        .filter(|key| !is_placeholder(key.reveal()))
    else {
        return Ok((request, None));
    };
    let default = match drivers::piped(&request.harness) {
        Ok(Piped::Codex) => CODEX_DEFAULT_BASE_URL,
        Ok(Piped::Claude) => CLAUDE_DEFAULT_BASE_URL,
        Ok(Piped::Acp | Piped::JsonlTurn | Piped::JsonlRpc | Piped::TextTurn) | Err(_) => {
            return Err(failure("launch_key_relay_unsupported"));
        }
    };
    let upstream = request.account.base_url().unwrap_or(default).to_owned();
    let secret = SecretString::new(key.reveal().as_bytes().to_vec())
        .map_err(|_| failure("launch_secret_invalid"))?;
    let relay = Relay::start(&upstream, secret).map_err(|error| LaunchError(error.0, None))?;
    let home = request.account.home().to_path_buf();
    request.account = AccountBinding::Url {
        home,
        base_url: relay.base_url(),
        key: Some(Secret::new(relay.token().to_owned())?),
    };
    assert!(
        request
            .account
            .key()
            .is_some_and(|key| is_placeholder(key.reveal()))
    );
    Ok((request, Some(relay)))
}

/// Routes a launch of a listed harness on a ChatGPT sign-in through a ChatGPT relay on
/// 127.0.0.1: the relay holds the sign-in's access token in engine memory and forwards
/// only Responses requests to the ChatGPT backend, and the returned request names the
/// relay's base URL and a random placeholder instead. The launch runs in the harness's own
/// private home made in the agent's home, never in the sign-in's managed home, where the
/// engine hands Codex the sign-in's own copy, and its sandbox is made after routing, for
/// that home. Codex and Claude Code keep their own sign-in paths. Inputs: the request bound
/// to the sign-in, with no sandbox profile yet, the harness, the stored sign-in, the
/// agent's own 0700 home and the sign-in source. Output: the request to launch and the
/// relay, which must live as long as the agent. A source read from another sign-in than
/// the stored one is a wiring fault and stops on an assertion before any relay starts.
/// Refusals: `launch_chatgpt_unsupported` for an own adapter, a harness that does not list
/// `openai-responses`, another account or a request not bound to a sign-in,
/// `launch_chatgpt_sandbox_premade` for a request that already carries a sandbox profile,
/// `launch_chatgpt_home_is_managed` for an agent home that is the sign-in's managed home,
/// lies inside it or holds it, `launch_account_home_holds_sign_in` where a sign-in's record
/// lies, or the private home's refusal.
pub fn route_chatgpt(
    mut request: LaunchRequest,
    harness: &super::registry::Harness,
    connection: &crate::config::ModelConnection,
    base: &Path,
    source: std::sync::Arc<crate::model_gateway::relay::chatgpt::ChatGptSource>,
) -> Result<(LaunchRequest, Relay), LaunchError> {
    let AccountBinding::Subscription { home: managed } = &request.account else {
        return Err(failure("launch_chatgpt_unsupported"));
    };
    let routed = harness.takes_chatgpt_sign_in()
        && harness.named(&request.harness)
        && super::registry::is_chatgpt_sign_in(connection);
    if !routed {
        return Err(failure("launch_chatgpt_unsupported"));
    }
    if request.sandbox_profile.is_some() {
        return Err(failure("launch_chatgpt_sandbox_premade"));
    }
    let crate::config::ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return Err(failure("launch_chatgpt_unsupported"));
    };
    assert!(
        source.profile() == profile_ref,
        "a ChatGPT relay serves only the sign-in it was read from"
    );
    if overlaps(base, managed) {
        return Err(failure("launch_chatgpt_home_is_managed"));
    }
    let home = chatgpt_home(base, harness, &connection.id)?;
    let relay = Relay::start_chatgpt(source).map_err(|error| LaunchError(error.0, None))?;
    request.account = AccountBinding::Url {
        home,
        base_url: relay.base_url(),
        key: Some(Secret::new(relay.token().to_owned())?),
    };
    assert!(relay.is_chatgpt());
    assert!(request.account.home().starts_with(base));
    assert!(
        request
            .account
            .key()
            .is_some_and(|key| is_placeholder(key.reveal()))
    );
    Ok((request, relay))
}

/// Whether two folders overlap: one is the other or lies inside it, as written or as their
/// real paths where they exist, so neither a link nor a folder made later hides it.
/// Inputs: the two folders. Output: the answer.
fn overlaps(left: &Path, right: &Path) -> bool {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let within = |left: &Path, right: &Path| left.starts_with(right) || right.starts_with(left);
    within(left, right) || within(&real(left), &real(right))
}

/// Makes the private home a ChatGPT launch runs in, `<agent home>/<harness id>/<account>`
/// (T18), refusing an agent home or a private home that holds a sign-in's record.
/// Inputs: the agent's own 0700 home, the harness and the account. Output: the home,
/// `launch_account_home_holds_sign_in`, or the private home's refusal.
#[cfg(unix)]
fn chatgpt_home(
    base: &Path,
    harness: &super::registry::Harness,
    account: &crate::protocol::CredentialRef,
) -> Result<PathBuf, LaunchError> {
    holds_no_sign_in(base)?;
    let home = super::home::private_home(base, &harness.manifest().id, account)?;
    holds_no_sign_in(&home)?;
    Ok(home)
}

/// Refuses a private home where its Unix owner and 0700 mode cannot be checked. Inputs:
/// unused. Output: `launch_account_home_unavailable`.
#[cfg(not(unix))]
fn chatgpt_home(
    base: &Path,
    harness: &super::registry::Harness,
    account: &crate::protocol::CredentialRef,
) -> Result<PathBuf, LaunchError> {
    let _ = (base, harness, account);
    Err(failure("launch_account_home_unavailable"))
}

/// Refuses a folder that holds a native sign-in's own record, Codex's or Claude Code's,
/// as a file, a link or anything else, so a listed harness never runs where it could read
/// a sign-in's tokens. Input: the folder. Output: Ok, or
/// `launch_account_home_holds_sign_in`, also when the folder cannot be looked into.
#[cfg(unix)]
pub(crate) fn holds_no_sign_in(folder: &Path) -> Result<(), LaunchError> {
    use crate::engine::bootstrap::{CLAUDE_RECORD, CODEX_RECORD};
    for record in [CODEX_RECORD, CLAUDE_RECORD] {
        match std::fs::symlink_metadata(folder.join(record)) {
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(failure("launch_account_home_holds_sign_in")),
        }
    }
    Ok(())
}

/// Builds the exact command a launch would spawn, without spawning it.
/// Input: the launch request. Output: the command, or a plain refusal code when an
/// input is unsafe or the harness has no launch adapter.
pub fn command(request: &LaunchRequest) -> Result<Command, LaunchError> {
    build(request, None)
}

/// Builds a context-less durable launch's command under its agent home's lock
/// ([`crate::launch_context::lock_only`]): a listed harness's, on the drivers that lock reads
/// as listed, with the variables its home block sets, Codex's and Claude Code's as
/// [`command`] builds them.
/// Input: the routed request. Output: the command and the lock guard, or the refusal.
fn lone_command(request: &LaunchRequest) -> Result<(Command, Installed), LaunchError> {
    let installed = crate::launch_context::lock_only(request.account.home(), &request.harness)?;
    let listed = matches!(
        drivers::piped(&request.harness),
        Ok(Piped::Acp | Piped::JsonlTurn | Piped::TextTurn)
    );
    let command = build(request, listed.then_some(&installed))?;
    Ok((command, installed))
}

/// Builds the command for an agent of one project: writes the project's approved skills,
/// MCP servers, brief, context index and approved memories into the agent home, then
/// points the harness at them. Input: the launch request and the project's context from
/// `launch_context::for_project`. Output: the command and the guard that removes the
/// generated files when dropped, or `launch_secret_in_command` when an MCP secret would be
/// in an argument or the environment.
pub fn command_with_context(
    request: &LaunchRequest,
    context: &AgentContext,
) -> Result<(Command, Installed), LaunchError> {
    validate(request)?;
    let installed = context.install(&request.harness, request.account.home(), request.tools)?;
    let command = build(request, Some(&installed))?;
    context.refuse_exposed(command.get_args().map(OsStr::as_encoded_bytes).chain(
        command.get_envs().flat_map(|(name, value)| {
            std::iter::once(name.as_encoded_bytes()).chain(value.map(OsStr::as_encoded_bytes))
        }),
    ))?;
    Ok((command, installed))
}

fn build(request: &LaunchRequest, installed: Option<&Installed>) -> Result<Command, LaunchError> {
    validate(request)?;
    if request
        .account
        .key()
        .is_some_and(|key| !is_placeholder(key.reveal()))
    {
        return Err(failure("launch_environment_refused"));
    }
    let home = request.account.home();
    let key_file = request
        .account
        .key()
        .map(|_| key_file_text(&home.join(KEY_FILE)))
        .transpose()?;
    let (arguments, environment) = match drivers::piped(&request.harness)? {
        Piped::Codex => codex_invocation(request, installed, key_file.as_deref())?,
        Piped::Claude => claude_invocation(request, installed, key_file.as_deref()),
        Piped::Acp => acp_invocation(request)?,
        Piped::JsonlTurn | Piped::JsonlRpc | Piped::TextTurn => {
            return Err(failure("launch_harness_unsupported"));
        }
    };
    sandboxed(request, installed, &arguments, &environment)
}

/// The command line of an ACP agent: its manifest's arguments filled for this launch, and no
/// variable of its own, since the installed context gives its home block's variables.
/// Input: the request. Output: the arguments and variables, or the refusal
/// [`drivers::acp::arguments`] gives.
fn acp_invocation(request: &LaunchRequest) -> Result<Invocation, LaunchError> {
    let listed = super::registry::registry()
        .get(&request.harness)
        .ok_or_else(|| failure("launch_harness_unsupported"))?;
    let arguments = drivers::acp::arguments(
        listed.manifest(),
        request.account.home(),
        &request.working_dir,
        request.model.as_deref(),
        request.tools,
    )?;
    Ok((
        arguments.into_iter().map(OsString::from).collect(),
        Vec::new(),
    ))
}

/// Wraps a harness command line in the launch's sandbox, with the agent's own environment:
/// its home, temporary folder, tool path, plain locale, the model proxy and git identity of
/// an installed context, whose tool home, if any, holds the temporary folder, own `git` and
/// cargo home in place of the binding's home (rv-code4 T19b F2), the harness's own variables, those a listed harness's home block
/// sets and those of its account hand-over (T19b 19b-2), whose placeholder goes through the
/// one exception to the variable guard, as a listed master's does. Inputs: the checked
/// request, the installed context, if any, the harness arguments and variables. Output: the
/// command with piped standard streams in the working folder, or
/// `launch_sandbox_unavailable` off macOS, `launch_account_handover_mismatch` for a
/// hand-over of another account than the binding's and `launch_environment_refused` when
/// the environment cannot be built, which includes a hand-over at Full access.
fn sandboxed(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    arguments: &[OsString],
    environment: &[(&'static str, OsString)],
) -> Result<Command, LaunchError> {
    let handover = installed.and_then(Installed::handover);
    if let Some(handover) = handover {
        handover_fits(request, handover)?;
    }
    let listed = installed.map(Installed::environment).unwrap_or_default();
    let handed_env = handover.map(Handover::env).unwrap_or_default();
    let harness_env = environment
        .iter()
        .map(|(name, value)| (OsStr::new(*name), Some(value.as_os_str())))
        .chain(
            listed
                .iter()
                .map(|(name, value)| (name.as_os_str(), Some(value.as_os_str()))),
        )
        .chain(
            handed_env
                .iter()
                .map(|(name, value)| (OsStr::new(name), Some(OsStr::new(value)))),
        );
    let names: Vec<&str> = handed_env
        .iter()
        .filter(|(_, value)| handover.is_some_and(|handover| value == handover.token()))
        .map(|(name, _)| name.as_str())
        .collect();
    let handed = handover
        .map(|handover| crate::sandbox::Handed::new(&names, handover.token()))
        .transpose()
        .map_err(|_| failure("launch_environment_refused"))?;
    assert_eq!(handed.is_some(), handover.is_some());
    confined(request, installed, arguments, harness_env, handed)
}

/// Refuses an account hand-over made for another account than the launch is bound to: its
/// address must be the binding's and its placeholder the binding's relay token, or the
/// local placeholder for a keyless binding. The terminal path checks its hand-over with it
/// too. Inputs: the request and the hand-over. Output: Ok, or
/// `launch_account_handover_mismatch`.
pub(super) fn handover_fits(
    request: &LaunchRequest,
    handover: &Handover,
) -> Result<(), LaunchError> {
    let address = request
        .account
        .base_url()
        .map(|url| url.trim_end_matches('/'));
    let token = request
        .account
        .key()
        .map_or(LOCAL_PLACEHOLDER_KEY, Secret::reveal);
    if address != Some(handover.base_url().trim_end_matches('/')) || token != handover.token() {
        return Err(failure("launch_account_handover_mismatch"));
    }
    Ok(())
}

/// Builds the command one listed master turn spawns (DEC-180 Task 20): the request's sandbox
/// and base variables as [`command`] gives them, the manifest's arguments, and the harness
/// variables with the launch's exception to the variable guard. Inputs: the request, the
/// arguments, the variables and the exception. Output: the command, or the refusals of
/// [`command`].
pub fn listed_command(
    request: &LaunchRequest,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    handed: crate::sandbox::Handed<'_>,
) -> Result<Command, LaunchError> {
    validate(request)?;
    let harness_env = environment
        .iter()
        .map(|(name, value)| (name.as_os_str(), Some(value.as_os_str())));
    confined(request, None, arguments, harness_env, Some(handed))
}

/// Wraps a harness command line as [`sandboxed`] says, with the launch's exception to the
/// variable guard, if any, which only a confined launch may carry. Inputs: the checked
/// request, the installed context, the harness arguments and variables and the exception.
/// Output: the command, or the refusals of [`sandboxed`].
fn confined<'a>(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    arguments: &[OsString],
    harness_env: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
    handed: Option<crate::sandbox::Handed<'_>>,
) -> Result<Command, LaunchError> {
    let home = request.account.home();
    let tool_home = installed.and_then(Installed::tool_home).unwrap_or(home);
    if !cfg!(target_os = "macos") {
        return Err(failure("launch_sandbox_unavailable"));
    }
    let homes = ToolHomes::owner().map_err(|_| failure("launch_environment_refused"))?;
    let path = toolchains::with_agent_git(
        tool_home,
        if request.tools {
            toolchains::agent_path(&homes, TOOLS_ON_PATH)
        } else {
            TOOLS_OFF_PATH.to_owned()
        },
    );
    let tmp = toolchains::existing_tmp(tool_home).unwrap_or_else(|| home.to_path_buf());
    let tools = if request.tools {
        toolchains::agent_env(tool_home, &homes)
    } else {
        Vec::new()
    };
    let mut base = vec![
        ("HOME", home.as_os_str()),
        ("TMPDIR", tmp.as_os_str()),
        ("PATH", OsStr::new(&path)),
        ("LANG", OsStr::new("C")),
        ("LC_ALL", OsStr::new("C")),
    ];
    base.extend(tools.iter().map(|(name, value)| (*name, value.as_os_str())));
    let proxy = installed
        .and_then(Installed::model_proxy)
        .map(crate::sandbox::model_proxy::variables);
    base.extend(
        proxy
            .iter()
            .flatten()
            .map(|(name, value)| (*name, OsStr::new(value))),
    );
    if let Some(identity) = &request.git_identity {
        base.extend(
            identity
                .variables()
                .map(|(name, value)| (name, OsStr::new(value))),
        );
    }
    let words = super::interactive::launch_words(&request.executable)?;
    let (program, script) = words
        .split_first()
        .ok_or_else(|| failure("launch_environment_refused"))?;
    let program = Path::new(program);
    let mut command = match (&request.sandbox_profile, handed) {
        #[cfg(unix)]
        (Some(profile), Some(handed)) => {
            crate::sandbox::agent_command_handing(profile, program, &base, harness_env, handed)
        }
        #[cfg(not(unix))]
        (Some(_), Some(_)) => return Err(failure("launch_sandbox_unavailable")),
        (Some(profile), None) => {
            crate::sandbox::agent_command(profile, program, &base, harness_env)
        }
        (None, None) => crate::sandbox::unconfined_command(program, &base, harness_env),
        (None, Some(_)) => return Err(failure("launch_environment_refused")),
    }
    .map_err(|_| failure("launch_environment_refused"))?;
    command
        .args(script)
        .args(arguments)
        .current_dir(&request.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_eq!(
        command.get_current_dir(),
        Some(request.working_dir.as_path())
    );
    assert_eq!(
        crate::sandbox::starts_at_boundary(command.get_program()),
        request.sandbox_profile.is_some() && !request.unconfined
    );
    assert!(command.get_args().all(|argument| {
        request
            .account
            .key()
            .is_none_or(|key| !argument.to_string_lossy().contains(&key.0))
    }));
    Ok(command)
}

/// The Codex app-server command line. Every folder of the working folder's walk is recorded
/// untrusted, so the app server neither loads a repository's `.codex` nor trusts a folder by
/// itself. Inputs: the request, the installed context, if any, and the key file. Output: the
/// arguments and variables, or `launch_working_dir_unavailable`.
fn codex_invocation(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    key_file: Option<&str>,
) -> Result<Invocation, LaunchError> {
    let home = request.account.home().as_os_str().to_owned();
    let environment = vec![("CODEX_HOME", home)];
    let folder = request
        .working_dir
        .canonicalize()
        .map_err(|_| failure("launch_working_dir_unavailable"))?;
    let mut arguments = Vec::new();
    for setting in codex_launch_settings(request, installed, key_file)
        .into_iter()
        .chain([super::codex_folder::untrusted(&folder)?])
    {
        arguments.push(OsString::from("-c"));
        arguments.push(OsString::from(setting));
    }
    arguments.extend(["app-server", "--listen", "stdio://"].map(OsString::from));
    Ok((arguments, environment))
}

/// Checks the path of an account's key file before a harness is told to read it: an
/// absolute path that fits inside a single-quoted shell word and a TOML string unchanged.
/// Input: the path. Output: its text, or `launch_key_file_invalid`.
pub(crate) fn key_file_text(path: &Path) -> Result<String, LaunchError> {
    let file = path
        .to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| failure("launch_key_file_invalid"))?;
    if !path.is_absolute() || file.contains(['\'', '"', '\\']) {
        return Err(failure("launch_key_file_invalid"));
    }
    Ok(file.to_owned())
}

/// Builds a Codex launch's `-c` settings from [`codex_configuration`] and what its installed
/// context asks for: a native lead (PA-7) leaves out the multi-agent v2 off switch, so its
/// `config.toml` turns Codex's own sub-agents on. Inputs: the request, the installed
/// context, if any, and the key file. Output: the settings, in order.
pub(super) fn codex_launch_settings(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    key_file: Option<&str>,
) -> Vec<String> {
    let native = installed.is_some_and(Installed::native_threads);
    assert!(!native || request.tools);
    let mut settings = codex_configuration(request, installed.is_some(), key_file);
    if native {
        settings.retain(|setting| setting != CODEX_MULTI_AGENT_V2_OFF);
    }
    assert_eq!(
        settings
            .iter()
            .any(|setting| setting == CODEX_MULTI_AGENT_V2_OFF),
        request.tools && !native
    );
    settings
}

/// Builds the `-c` settings every Codex launch gets: file credentials in the account home,
/// the account's provider, the model and the tools switch. A keyed provider reads its
/// relay token by running `/bin/cat` on `key_file` (see [`key_file_text`]), so the token
/// is in no argument or variable. Input: the request, whether project context was
/// installed and the key file of a keyed account. Output: the settings, in order.
pub(super) fn codex_configuration(
    request: &LaunchRequest,
    context: bool,
    key_file: Option<&str>,
) -> Vec<String> {
    assert_eq!(request.account.key().is_some(), key_file.is_some());
    let mut configuration = vec!["cli_auth_credentials_store=\"file\"".to_owned()];
    match &request.account {
        AccountBinding::Subscription { .. } => {
            configuration.push("model_provider=\"openai\"".into());
        }
        AccountBinding::ApiKey { .. } | AccountBinding::Url { .. } => {
            let base_url = request.account.base_url().unwrap_or(CODEX_DEFAULT_BASE_URL);
            let key = key_file.map_or(String::new(), |file| {
                format!(",auth={{command=\"/bin/cat\",args=[\"{file}\"]}}")
            });
            configuration.push(format!(
                "model_providers.crazyagent={{name=\"CrazyAgent\",base_url=\"{base_url}\",wire_api=\"responses\"{key}}}"
            ));
            configuration.push("model_provider=\"crazyagent\"".into());
        }
    }
    let provider = match &request.account {
        AccountBinding::Subscription { .. } => crate::provider_retry::CodexProvider::BuiltIn,
        AccountBinding::ApiKey { .. } | AccountBinding::Url { .. } => {
            crate::provider_retry::CodexProvider::Custom("crazyagent")
        }
    };
    configuration.extend(request.limits.provider_retry.codex_settings(provider));
    if let Some(model) = &request.model {
        configuration.push(format!("model=\"{model}\""));
    }
    let tools: &[&str] = if request.tools {
        &CODEX_TOOLS_ON
    } else {
        &CODEX_TOOLS_OFF
    };
    configuration.extend(
        tools
            .iter()
            .filter(|setting| !(context && request.tools && **setting == "mcp_servers={}"))
            .map(|setting| (*setting).to_owned()),
    );
    assert!(request.account.key().is_none_or(|key| {
        configuration
            .iter()
            .all(|setting| !setting.contains(&key.0))
    }));
    configuration
}

/// Claude Code's `--settings` for one launch: the fixed settings, plus for a keyed account
/// an `apiKeyHelper` that prints its relay token from `key_file` (see [`key_file_text`]),
/// and without `disableAllHooks` when the launch's generated plugin holds the hooks the
/// owner turned on. Setting sources stay off, so no other hook loads. Inputs: the key file
/// of a keyed account and whether the launch runs hooks. Output: the settings JSON.
fn claude_settings(key_file: Option<&str>, hooks: bool) -> String {
    if key_file.is_none() && !hooks {
        return CLAUDE_SETTINGS.to_owned();
    }
    let mut settings: serde_json::Value =
        serde_json::from_str(CLAUDE_SETTINGS).expect("fixed Claude settings are JSON");
    if let Some(key_file) = key_file {
        settings["apiKeyHelper"] = format!("/bin/cat '{key_file}'").into();
    }
    if hooks {
        let removed = settings
            .as_object_mut()
            .and_then(|fields| fields.remove("disableAllHooks"));
        assert_eq!(removed, Some(serde_json::Value::Bool(true)));
    }
    settings.to_string()
}

/// Tells whether Claude Code starts `--bare` on an account: every account but a signed-in
/// subscription, so a key or endpoint account never authenticates through an OAuth login or
/// the keychain. Claude Code 2.1.309 fires no settings or plugin hooks under `--bare`
/// (DEC-104 bare-hooks). Input: the account. Output: the answer.
pub fn claude_bare(account: &AccountBinding) -> bool {
    !matches!(account, AccountBinding::Subscription { .. })
}

/// Lists what a Claude Code launch passes after `--disallowedTools`: with tools on, Claude
/// Code's own sub-agent tools, less `CLAUDE_NATIVE_TEAM_TOOLS` for a native-team lead, then
/// every MCP tool the owner switched off. Input: the tools switch, whether the launch is a
/// native-team lead and what it installed. Output: the names, empty when none is denied.
pub(crate) fn claude_denied(
    tools: bool,
    native: bool,
    installed: Option<&Installed>,
) -> Vec<String> {
    assert!(!native || tools);
    let mut denied: Vec<String> = Vec::new();
    if tools {
        denied.extend(
            CLAUDE_BUILT_IN_AGENTS
                .iter()
                .filter(|tool| !(native && CLAUDE_NATIVE_TEAM_TOOLS.contains(tool)))
                .map(|tool| (*tool).to_owned()),
        );
    }
    if let Some(installed) = installed {
        denied.extend(installed.denied_tools().iter().cloned());
    }
    assert!(
        denied
            .iter()
            .all(|name| !name.is_empty() && !name.starts_with('-'))
    );
    denied
}

fn claude_invocation(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    key_file: Option<&str>,
) -> Invocation {
    assert_eq!(request.account.key().is_some(), key_file.is_some());
    let home = request.account.home().as_os_str().to_owned();
    let settings = claude_settings(key_file, installed.is_some_and(Installed::runs_hooks));
    let keep = installed.is_some_and(Installed::keeps_transcript);
    let mut arguments: Vec<OsString> = CLAUDE_COMMON
        .iter()
        .filter(|argument| !(keep && **argument == CLAUDE_NO_TRANSCRIPT))
        .map(|argument| {
            if *argument == CLAUDE_SETTINGS {
                OsString::from(&settings)
            } else {
                OsString::from(argument)
            }
        })
        .collect();
    match installed {
        Some(installed) => arguments.extend(installed.arguments().iter().cloned()),
        None => arguments.extend(CLAUDE_NO_MCP.iter().map(OsString::from)),
    }
    if let Some(model) = &request.model {
        arguments.extend([OsString::from("--model"), OsString::from(model)]);
    }
    if request.tools {
        arguments.extend(
            [
                "--tools",
                "default",
                "--permission-mode",
                "bypassPermissions",
            ]
            .map(OsString::from),
        );
    } else {
        arguments.extend(CLAUDE_TOOLS_OFF.map(OsString::from));
    }
    let denied = claude_denied(request.tools, false, installed);
    if !denied.is_empty() {
        arguments.push(OsString::from(CLAUDE_DENY_FLAG));
        arguments.extend(denied.into_iter().map(OsString::from));
    }
    let mut environment = vec![
        ("CLAUDE_CONFIG_DIR", home.clone()),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR", home.clone()),
        ("CLAUDE_CODE_TMPDIR", home),
        ("TERM", OsString::from("dumb")),
        ("DISABLE_TELEMETRY", OsString::from("1")),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            OsString::from("1"),
        ),
        (
            CLAUDE_AUTO_MEMORY_OFF.0,
            OsString::from(CLAUDE_AUTO_MEMORY_OFF.1),
        ),
    ];
    if let Some((name, value)) = request.limits.provider_retry.claude_env() {
        environment.push((name, OsString::from(value)));
    }
    if request.tools {
        environment.push(("SHELL", OsString::from("/bin/zsh")));
        environment.push((
            CLAUDE_BACKGROUND_TASKS_OFF.0,
            OsString::from(CLAUDE_BACKGROUND_TASKS_OFF.1),
        ));
        environment.push((
            CLAUDE_MCP_CONNECT_LIMIT.0,
            OsString::from(CLAUDE_MCP_CONNECT_LIMIT.1),
        ));
    }
    if claude_bare(&request.account) {
        arguments.push(OsString::from("--bare"));
        if key_file.is_none() {
            environment.push(("ANTHROPIC_API_KEY", OsString::from(LOCAL_PLACEHOLDER_KEY)));
        }
        if let Some(base_url) = request.account.base_url() {
            environment.push(("ANTHROPIC_BASE_URL", OsString::from(base_url)));
        }
        if let Some(model) = &request.model {
            environment.push(("ANTHROPIC_SMALL_FAST_MODEL", OsString::from(model)));
        }
    }
    (arguments, environment)
}

pub(super) fn harness_id(request: &LaunchRequest) -> &'static str {
    super::spec(&request.harness).map_or("", |spec| spec.id)
}

pub(super) fn validate(request: &LaunchRequest) -> Result<(), LaunchError> {
    let executable = std::fs::metadata(&request.executable)
        .map_err(|_| failure("launch_executable_unavailable"))?;
    if !request.executable.is_absolute() || !executable.is_file() || !super::runnable(&executable) {
        return Err(failure("launch_executable_unavailable"));
    }
    if !request.working_dir.is_absolute() || !request.working_dir.is_dir() {
        return Err(failure("launch_working_dir_unavailable"));
    }
    validate_home(request.account.home())?;
    if let Some(model) = &request.model
        && (model.is_empty()
            || model.len() > 256
            || !model.as_bytes()[0].is_ascii_alphanumeric()
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte)))
    {
        return Err(failure("launch_model_invalid"));
    }
    if let Some(base_url) = request.account.base_url() {
        let parsed = url::Url::parse(base_url).map_err(|_| failure("launch_url_invalid"))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.as_str().trim_end_matches('/') != base_url.trim_end_matches('/')
            || base_url.len() > 2048
        {
            return Err(failure("launch_url_invalid"));
        }
    }
    match (&request.sandbox_profile, request.unconfined) {
        (Some(_), true) => return Err(failure("launch_sandbox_conflict")),
        (Some(profile), false) if !profile.is_absolute() || !profile.is_file() => {
            return Err(failure("launch_sandbox_profile_unavailable"));
        }
        (Some(_), false) | (None, true) => {}
        (None, false) => return Err(failure("launch_sandbox_required")),
    }
    let limits = request.limits;
    if limits.run_time.is_zero()
        || limits.run_time > RUN_TIME_MAX
        || limits.stop_grace > Duration::from_secs(30)
    {
        return Err(failure("launch_limits_invalid"));
    }
    Ok(())
}

/// Checks that an account home is safe to launch in: an absolute 0700 directory owned by
/// this user that is neither the owner's HOME nor a folder any harness manifest names as
/// the owner's own (`home.owner_folders`, such as `~/.codex`). Input: the home. Output: Ok,
/// or a plain `launch_account_home_*` refusal code.
#[cfg(unix)]
pub(crate) fn validate_home(home: &Path) -> Result<(), LaunchError> {
    let metadata =
        std::fs::symlink_metadata(home).map_err(|_| failure("launch_account_home_unavailable"))?;
    if !home.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(failure("launch_account_home_unsafe"));
    }
    let canonical =
        std::fs::canonicalize(home).map_err(|_| failure("launch_account_home_unavailable"))?;
    if let Some(owner) = std::env::var_os("HOME").map(PathBuf::from) {
        let folders = crate::sandbox::harness_folders().map(|folder| owner.join(folder));
        for forbidden in std::iter::once(owner.clone()).chain(folders) {
            if std::fs::canonicalize(&forbidden).is_ok_and(|forbidden| forbidden == canonical)
                || forbidden == home
            {
                return Err(failure("launch_account_home_forbidden"));
            }
        }
    }
    Ok(())
}

/// Refuses an account home where its Unix owner and 0700 mode cannot be checked.
/// Takes the home, unused. Returns `launch_account_home_unavailable`.
#[cfg(not(unix))]
pub(crate) fn validate_home(_home: &Path) -> Result<(), LaunchError> {
    Err(failure("launch_account_home_unavailable"))
}

/// What a harness output reader hands the session: one line, the note that a line longer
/// than `LINE_BYTES_MAX` was passed over, or the end of the output.
enum Line {
    Bytes(Vec<u8>),
    Closed,
    Skipped,
}

/// The most output lines a reader holds for the session before it waits for the session to
/// take one, so a session that reads slowly holds a bounded amount of output.
const LINES_AHEAD_MAX: usize = 16;

enum Driver {
    Codex {
        protocol: Box<CodexProtocol>,
        cwd: PathBuf,
        tools: bool,
        prompt: Option<String>,
        interrupt: bool,
        server_turn: bool,
        native_threads: bool,
    },
    Claude {
        protocol: Box<ClaudeProtocol>,
        turns: u64,
        interrupt: Option<String>,
        late: u64,
    },
    Acp {
        protocol: Box<Acp>,
    },
}

impl Driver {
    /// Starts the protocol driver of one headless launch: a Codex native lead (PA-7) keeps
    /// the sub-agent threads its Codex starts, any other Codex launch interrupts each one,
    /// and an ACP launch opens its session with the engine's own MCP servers.
    /// Inputs: the request, whether it is a Codex native lead and the engine's servers.
    /// Output: the driver and its first frames, or a plain refusal code.
    fn new(
        request: &LaunchRequest,
        native_threads: bool,
        servers: &[EngineServer],
    ) -> Result<(Self, Vec<Vec<u8>>), LaunchError> {
        let piped = drivers::piped(&request.harness);
        assert!(!native_threads || (matches!(piped, Ok(Piped::Codex)) && request.tools));
        match piped? {
            Piped::Codex => {
                let mut protocol = CodexProtocol::new();
                let start = protocol
                    .initialize()
                    .map_err(|_| failure("launch_protocol_failed"))?;
                let driver = Self::Codex {
                    protocol: Box::new(protocol),
                    cwd: request.working_dir.clone(),
                    tools: request.tools,
                    prompt: None,
                    interrupt: false,
                    server_turn: false,
                    native_threads,
                };
                Ok((driver, vec![start]))
            }
            Piped::Claude => {
                let local = |name: &str| format!("launch-{name}-{}", std::process::id());
                let action = NativeAction::Launch {
                    session_id: SessionId::new(local("session"))
                        .map_err(|_| failure("launch_identity"))?,
                    harness: NativeHarness::Claude,
                    profile_ref: CredentialRef::new(local("profile"))
                        .map_err(|_| failure("launch_identity"))?,
                    worktree_lease: RequestId::new(local("lease"))
                        .map_err(|_| failure("launch_identity"))?,
                };
                let account =
                    CredentialRef::new(local("account")).map_err(|_| failure("launch_identity"))?;
                let protocol = ClaudeProtocol::new(action, account)
                    .map_err(|_| failure("launch_protocol_failed"))?;
                Ok((
                    Self::Claude {
                        protocol: Box::new(protocol),
                        turns: 0,
                        interrupt: None,
                        late: 0,
                    },
                    Vec::new(),
                ))
            }
            Piped::Acp => {
                let (protocol, start) = Acp::new(&request.working_dir, request.tools, None)?;
                let driver = Self::Acp {
                    protocol: Box::new(protocol.with_servers(servers)),
                };
                Ok((driver, vec![start]))
            }
            Piped::JsonlTurn | Piped::JsonlRpc | Piped::TextTurn => {
                Err(failure("launch_harness_unsupported"))
            }
        }
    }

    fn send(&mut self, text: &str) -> Result<Option<Vec<u8>>, LaunchError> {
        if text.trim().is_empty() || text.contains('\0') || text.len() > PROMPT_BYTES_MAX {
            return Err(failure("launch_prompt_invalid"));
        }
        match self {
            Self::Codex {
                protocol, prompt, ..
            } => match protocol.state() {
                CodexState::Idle => protocol
                    .start_turn(text)
                    .map(Some)
                    .map_err(|_| failure("launch_protocol_failed")),
                CodexState::New
                | CodexState::Initializing
                | CodexState::Initialized
                | CodexState::Ready
                | CodexState::ThreadStarting
                    if prompt.is_none() =>
                {
                    *prompt = Some(text.to_owned());
                    Ok(None)
                }
                _ => Err(failure("launch_busy")),
            },
            Self::Claude {
                protocol, turns, ..
            } => {
                if !matches!(
                    protocol.state(),
                    ClaudeState::AwaitingInit | ClaudeState::Idle
                ) {
                    return Err(failure("launch_busy"));
                }
                *turns += 1;
                let request = RequestId::new(format!("turn-{turns}"))
                    .map_err(|_| failure("launch_identity"))?;
                protocol
                    .start_turn(request, text)
                    .map(Some)
                    .map_err(|_| failure("launch_busy"))
            }
            Self::Acp { protocol } => protocol.prompt(text),
        }
    }

    fn interrupt(&mut self) -> Result<Option<Vec<u8>>, LaunchError> {
        match self {
            Self::Codex {
                protocol,
                interrupt,
                server_turn,
                ..
            } => match protocol.state() {
                CodexState::Active if *server_turn => protocol
                    .interrupt()
                    .map(Some)
                    .map_err(|_| failure("launch_protocol_failed")),
                CodexState::TurnStarting | CodexState::Active => {
                    *interrupt = true;
                    Ok(None)
                }
                _ => Err(failure("launch_not_running")),
            },
            Self::Claude {
                protocol,
                turns,
                interrupt,
                ..
            } => {
                if protocol.state() != ClaudeState::Active || interrupt.is_some() {
                    return Err(failure("launch_not_running"));
                }
                let id = format!("interrupt-{turns}");
                let mut bytes = serde_json::to_vec(&serde_json::json!({
                    "type":"control_request","request_id":id,"request":{"subtype":"interrupt"}
                }))
                .map_err(|_| failure("launch_protocol_failed"))?;
                bytes.push(b'\n');
                *interrupt = Some(id);
                Ok(Some(bytes))
            }
            Self::Acp { protocol } => protocol.cancel(),
        }
    }

    fn receive(&mut self, line: &[u8]) -> Result<(Vec<LaunchEvent>, Vec<Vec<u8>>), LaunchError> {
        let mut framed = line.to_vec();
        if framed.last() != Some(&b'\n') {
            framed.push(b'\n');
        }
        let mut events = Vec::new();
        let mut writes = Vec::new();
        match self {
            Self::Codex {
                protocol,
                cwd,
                tools,
                prompt,
                interrupt,
                server_turn,
                native_threads,
            } => {
                let method = crate::adapters::claude::parse_line(line)
                    .map_err(|_| failure("launch_protocol_failed"))?;
                let started = method.get("method").and_then(serde_json::Value::as_str)
                    == Some("turn/started");
                let decoded = protocol
                    .receive(&framed)
                    .map_err(|_| failure("launch_protocol_failed"))?;
                if started
                    && !decoded
                        .iter()
                        .any(|event| matches!(event, CodexEvent::NativeSubAgentStarted { .. }))
                {
                    *server_turn = true;
                }
                for event in decoded {
                    match event {
                        CodexEvent::Initialized => {
                            writes.push(
                                protocol
                                    .initialized()
                                    .map_err(|_| failure("launch_protocol_failed"))?,
                            );
                            let start = if *tools {
                                protocol.start_tool_thread(cwd)
                            } else {
                                protocol.start_thread(cwd)
                            };
                            writes.push(start.map_err(|_| failure("launch_protocol_failed"))?);
                        }
                        CodexEvent::ThreadStarted { thread_id } => {
                            events.push(LaunchEvent::Ready {
                                native_session: Some(thread_id),
                            });
                            if let Some(text) = prompt.take() {
                                writes.push(
                                    protocol
                                        .start_turn(&text)
                                        .map_err(|_| failure("launch_protocol_failed"))?,
                                );
                            }
                        }
                        CodexEvent::AgentMessageDelta { delta, .. } => {
                            events.push(LaunchEvent::Text { text: delta });
                        }
                        CodexEvent::TurnCompleted { status, .. } => {
                            *interrupt = false;
                            *server_turn = false;
                            if status == TurnStatus::Interrupted {
                                events.push(LaunchEvent::Interrupted);
                            }
                            events.push(LaunchEvent::TurnFinished {
                                success: status == TurnStatus::Completed,
                            });
                        }
                        CodexEvent::NativeSubAgentStarted { .. } if *native_threads => {}
                        CodexEvent::NativeSubAgentStarted { thread_id, turn_id } => {
                            writes.push(
                                protocol
                                    .interrupt_child(&thread_id, &turn_id)
                                    .map_err(|_| failure("launch_protocol_failed"))?,
                            );
                        }
                        CodexEvent::ApprovalRequired { id, .. }
                        | CodexEvent::UnsupportedRequest { id, .. } => {
                            writes.push(
                                protocol
                                    .deny_request(&id)
                                    .map_err(|_| failure("launch_protocol_failed"))?,
                            );
                            events.push(LaunchEvent::ApprovalDenied);
                        }
                        CodexEvent::TurnError { message, .. } => {
                            events.push(LaunchEvent::TurnError {
                                rate_limited: rate_limited(&message),
                            });
                        }
                        CodexEvent::RequestFailed {
                            method: "turn/interrupt",
                            ..
                        } => {}
                        CodexEvent::RequestFailed { .. } => {
                            return Err(failure("launch_request_failed"));
                        }
                        _ => {}
                    }
                }
                if *interrupt && *server_turn && protocol.state() == CodexState::Active {
                    *interrupt = false;
                    writes.push(
                        protocol
                            .interrupt()
                            .map_err(|_| failure("launch_protocol_failed"))?,
                    );
                }
            }
            Self::Claude {
                protocol,
                interrupt,
                late,
                ..
            } => {
                let value = crate::adapters::claude::parse_line(line)
                    .map_err(|_| failure("launch_protocol_failed"))?;
                match value.get("type").and_then(serde_json::Value::as_str) {
                    Some("control_response") => {
                        let answered = value
                            .pointer("/response/request_id")
                            .and_then(serde_json::Value::as_str);
                        if interrupt.is_some() && answered == interrupt.as_deref() {
                            events.push(LaunchEvent::Interrupted);
                        }
                    }
                    Some("control_request" | "keep_alive") => {}
                    _ if protocol.state() == ClaudeState::Idle => {
                        *late = late.saturating_add(1);
                        events.push(LaunchEvent::LateOutput { lines: *late });
                    }
                    _ => {
                        let decoded = protocol
                            .receive(&framed)
                            .map_err(|_| failure("launch_protocol_failed"))?;
                        for event in decoded {
                            match event.kind {
                                EventKind::Initialized { .. } => events.push(LaunchEvent::Ready {
                                    native_session: Some(event.native_session_id),
                                }),
                                EventKind::AssistantText { text, .. } => {
                                    events.push(LaunchEvent::Text { text })
                                }
                                EventKind::ToolUse { name, .. } => {
                                    events.push(LaunchEvent::ToolUse { name })
                                }
                                EventKind::AssistantError { code } => {
                                    events.push(LaunchEvent::TurnError {
                                        rate_limited: rate_limited(&code),
                                    })
                                }
                                EventKind::RateLimitObserved {
                                    status: RateLimitStatus::Rejected,
                                    ..
                                } => events.push(LaunchEvent::TurnError { rate_limited: true }),
                                EventKind::TurnFinished { outcome, usage, .. } => {
                                    *interrupt = None;
                                    let tokens = Tokens::reported(&usage);
                                    if !tokens.is_empty() {
                                        events.push(LaunchEvent::Usage { tokens });
                                    }
                                    events.push(LaunchEvent::TurnFinished {
                                        success: outcome == TurnOutcome::Success,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Self::Acp { protocol } => {
                let (decoded, answers) = protocol.receive(line)?;
                events.extend(decoded);
                writes.extend(answers);
            }
        }
        Ok((events, writes))
    }
}

/// A running harness process driven through its headless interface, leading its own process
/// group with a warden in its sandbox, so stop reaches every descendant, sessions included.
/// `uncertain` is set once its warden did not confirm that sandbox empty; `follower` follows a
/// Full access run's processes, which no warden reaches. One on an engine key relay holds the
/// engine's lifeline, so its group ends with the engine (b6 r2 F1), released once stopped.
pub struct Session {
    child: Child,
    warden: Option<crate::sandbox::warden::Warden>,
    uncertain: bool,
    follower: Option<Follower>,
    stdin: Option<File>,
    pending: Vec<u8>,
    lines: Receiver<Line>,
    driver: Driver,
    queue: VecDeque<LaunchEvent>,
    limits: LaunchLimits,
    deadline: Instant,
    exited: bool,
    stopped: bool,
    _context: Option<Installed>,
    _relay: Option<Relay>,
    _key: Option<KeyFile>,
    #[cfg(unix)]
    lifeline: Option<crate::research::runner::Lifeline>,
}

impl Session {
    /// Spawns the harness for a request and begins its handshake; a real provider key goes
    /// through [`route`]. Input: the launch request. Output: a running session, or a plain
    /// refusal code.
    pub fn start(request: LaunchRequest) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let key = write_key_file(&request)?;
        let command = command(&request)?;
        Self::spawn(request, command, None, None, None, (relay, key), &[])
    }

    /// Spawns one listed master turn's harness from the command [`listed_command`] built, its
    /// ACP session opened with the given engine servers, holding the relay of a keyed account
    /// and with it the engine's lifeline while the session lives (DEC-180 Task 20, T20 r1 F2).
    /// Inputs: the checked request, the command, the relay, if any, and the servers. Output:
    /// a running session, or a plain refusal code.
    pub fn start_listed(
        request: LaunchRequest,
        command: Command,
        relay: Option<Relay>,
        servers: &[EngineServer],
    ) -> Result<Self, LaunchError> {
        Self::spawn(request, command, None, None, None, (relay, None), servers)
    }

    /// Spawns the harness for an agent of one project with the project's approved skills,
    /// MCP servers, brief, context index and approved memories, the way every engine launch
    /// for a project should. Input: the launch request and `launch_context::for_project`.
    /// Output: a running session; its generated files are removed when it stops.
    pub fn start_with_context(
        request: LaunchRequest,
        context: AgentContext,
    ) -> Result<Self, LaunchError> {
        Self::start_context(request, context, None)
    }

    /// Spawns the harness as [`Self::start_with_context`] does and keeps its raw output in
    /// the run's logs, as a research run on pipes keeps it (T19b 19b-4, QUESTION-0151).
    /// Inputs: the launch request, the project context and the logs. Output: a running
    /// session, or a plain refusal code.
    pub fn start_logged(
        request: LaunchRequest,
        context: AgentContext,
        logs: RunLogs,
    ) -> Result<Self, LaunchError> {
        Self::start_context(request, context, Some(logs))
    }

    /// [`Self::start_with_context`] with the run's logs, if any. Inputs: the request, the
    /// context and the logs. Output: a running session, or a plain refusal code.
    fn start_context(
        request: LaunchRequest,
        context: AgentContext,
        logs: Option<RunLogs>,
    ) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let (command, installed) = command_with_context(&request, &context)?;
        drop(context);
        let key = write_key_file(&request)?;
        Self::spawn(
            request,
            command,
            Some(installed),
            None,
            logs,
            (relay, key),
            &[],
        )
    }

    /// Spawns a harness that outlives the engine, unless it is on an engine key relay it needs
    /// (b6 r2 F1): its input is a FIFO it holds open, so it never sees end of input when the
    /// engine dies, and its output goes to a private log whose lock it holds while any process
    /// of its group lives. Input: the request, the project context for a project agent and the
    /// agent's empty 0700 state directory. Output: a running session, or a plain refusal.
    pub fn start_durable(
        request: LaunchRequest,
        context: Option<AgentContext>,
        state: &Path,
    ) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let (command, installed) = match &context {
            Some(context) => command_with_context(&request, context)?,
            None => lone_command(&request)?,
        };
        drop(context);
        let io = DurableIo::create(state)?;
        let key = write_key_file(&request)?;
        let installed = Some(installed);
        Self::spawn(
            request,
            command,
            installed,
            Some(io),
            None,
            (relay, key),
            &[],
        )
    }

    fn spawn(
        request: LaunchRequest,
        mut command: Command,
        context: Option<Installed>,
        durable: Option<DurableIo>,
        logs: Option<RunLogs>,
        (relay, key): (Option<Relay>, Option<KeyFile>),
        listed: &[EngineServer],
    ) -> Result<Self, LaunchError> {
        assert!(context.is_none() || listed.is_empty());
        assert!(durable.is_none() || logs.is_none());
        let native_threads = context.as_ref().is_some_and(Installed::native_threads);
        let servers = context.as_ref().map_or(listed, Installed::engine_servers);
        let (driver, startup) = Driver::new(&request, native_threads, servers)?;
        let token = LineageToken::new().ok_or_else(|| failure("launch_spawn_failed"))?;
        command.env(LINEAGE_VARIABLE, token.as_str());
        let (sender, lines) = sync_channel(LINES_AHEAD_MAX);
        let mut child;
        let mut warden = None;
        let stdin;
        match durable {
            Some(io) => {
                command
                    .stdin(Stdio::from(io.child_input))
                    .stdout(Stdio::from(io.child_output))
                    .stderr(Stdio::null());
                child = crate::spawn_gate::spawn(&mut command)
                    .map_err(|_| failure("launch_spawn_failed"))?;
                drop(command);
                stdin = Some(io.input);
                let (output, probe) = (io.output, io.probe);
                std::thread::Builder::new()
                    .name("harness-output".into())
                    .spawn(move || tail(output, probe, sender))
                    .map_err(|_| failure("launch_reader_unavailable"))?;
            }
            None => {
                (child, warden) = crate::sandbox::warden::spawn(&mut command)
                    .map_err(|_| failure("launch_spawn_failed"))?;
                stdin = child.stdin.take().map(stdin_file);
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| failure("launch_stdio_unavailable"))?;
                let stderr = child
                    .stderr
                    .take()
                    .ok_or_else(|| failure("launch_stdio_unavailable"))?;
                let (events, mut errors) = match logs {
                    Some(logs) => (
                        Some(Capped::new(logs.events, logs.bytes_max)),
                        Some(Capped::new(logs.errors, logs.bytes_max)),
                    ),
                    None => (None, None),
                };
                std::thread::Builder::new()
                    .name("harness-stdout".into())
                    .spawn(move || forward_logged(stdout, sender, events))
                    .map_err(|_| failure("launch_reader_unavailable"))?;
                std::thread::Builder::new()
                    .name("harness-stderr".into())
                    .spawn(move || {
                        let mut stderr = stderr;
                        let mut buffer = [0_u8; 4096];
                        while let Ok(count) = stderr.read(&mut buffer) {
                            if count == 0 {
                                break;
                            }
                            if let Some(errors) = errors.as_mut() {
                                errors.write(&buffer[..count]);
                            }
                        }
                    })
                    .map_err(|_| failure("launch_reader_unavailable"))?;
            }
        }
        let check = if request.unconfined {
            crate::sandbox::LineageCheck::FullAccess
        } else {
            crate::sandbox::LineageCheck::Unneeded
        };
        let follower = Follower::start(child.id(), Some(token), check);
        #[cfg(unix)]
        let relayed = relay.is_some()
            || request
                .account
                .key()
                .is_some_and(|key| crate::model_gateway::relay::is_relay_token(key.reveal()));
        let mut session = Self {
            child,
            warden,
            uncertain: false,
            follower,
            stdin,
            pending: Vec::new(),
            lines,
            driver,
            queue: VecDeque::new(),
            limits: request.limits,
            deadline: Instant::now() + request.limits.run_time,
            exited: false,
            stopped: false,
            _context: context,
            _relay: relay,
            _key: key,
            #[cfg(unix)]
            lifeline: None,
        };
        #[cfg(unix)]
        if relayed {
            let lifeline = crate::research::runner::Lifeline::watch(session.child.id())
                .map_err(|_| failure("launch_spawn_failed"))?;
            session.lifeline = Some(lifeline);
        }
        #[cfg(unix)]
        if let Some(input) = &session.stdin {
            never_waits(input)?;
        }
        for bytes in startup {
            session.write(&bytes)?;
        }
        assert!(!session.stopped);
        Ok(session)
    }

    /// Returns the process group id that `stop` signals.
    pub fn process_group(&self) -> u32 {
        self.child.id()
    }

    /// Returns how long a stop waits for the harness before it is killed, the request's
    /// stop grace, which a turn asked to stop also waits on for the harness to end it.
    pub fn stop_grace(&self) -> Duration {
        self.limits.stop_grace
    }

    /// Sends one prompt. A prompt sent before the harness is ready is held until it is.
    /// Input: prompt text. Output: Ok, or `launch_busy` while a turn is running.
    pub fn send(&mut self, prompt: &str) -> Result<(), LaunchError> {
        self.require_live()?;
        if let Some(bytes) = self.driver.send(prompt)? {
            self.write(&bytes)?;
        }
        Ok(())
    }

    /// Asks the harness to stop the current turn through its own protocol.
    /// Output: Ok once the request is written, or `launch_not_running` with no turn.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        self.require_live()?;
        if let Some(bytes) = self.driver.interrupt()? {
            self.write(&bytes)?;
        }
        Ok(())
    }

    /// Waits up to `wait` for the next event. A line too long to hold is passed over with
    /// one note in the engine log. Output: Some(event), None on timeout, or an error once
    /// the session deadline passes or the output is malformed.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        let until = Instant::now() + wait;
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            if self.exited {
                return Err(failure("launch_closed"));
            }
            let now = Instant::now();
            if now >= self.deadline {
                self.stop_group();
                return Err(failure("launch_deadline"));
            }
            self.flush_input()?;
            if now >= until {
                return Ok(None);
            }
            let wait = until.min(self.deadline) - now;
            let wait = if self.pending.is_empty() {
                wait
            } else {
                wait.min(INPUT_RETRY)
            };
            match self.lines.recv_timeout(wait) {
                Ok(Line::Bytes(line)) => {
                    let (events, writes) = match self.driver.receive(&line) {
                        Ok(result) => result,
                        Err(error) => {
                            self.stop_group();
                            return Err(error);
                        }
                    };
                    for bytes in writes {
                        self.write(&bytes)?;
                    }
                    self.queue.extend(events);
                }
                Ok(Line::Skipped) => {
                    eprintln!("launch_output_line_skipped over={LINE_BYTES_MAX}");
                }
                Ok(Line::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    self.exited = true;
                    let code = self.reap(self.limits.stop_grace.max(Duration::from_millis(500)));
                    self.queue.push_back(LaunchEvent::Exited { code });
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Stops the whole process group: TERM, a bounded grace wait, then KILL, and then has the
    /// run's warden, when it has one, stop every process left in its sandbox. A Full access
    /// run has no warden, so the processes its lineage followed from the start up to the kill,
    /// and those carrying its launch's token, are checked after it. Output: the leader's exit
    /// code when it exited normally, [`Unstopped::Uncertain`] when a warden did not confirm
    /// that its sandbox is empty, or [`Unstopped::Left`] naming what a Full access run left
    /// running.
    pub fn stop(mut self) -> Result<Option<i32>, Unstopped> {
        let lineage = self.follower.take().map(Follower::stop);
        let code = self.stop_group();
        assert!(self.stopped && self.warden.is_none());
        if self.uncertain {
            return Err(Unstopped::Uncertain);
        }
        if let Some(lineage) = lineage {
            lineage?.settle()?;
        }
        Ok(code)
    }

    fn require_live(&self) -> Result<(), LaunchError> {
        if self.exited || self.stopped {
            return Err(failure("launch_closed"));
        }
        Ok(())
    }

    /// Hands bytes to the harness's input without ever waiting on it: what its pipe has room
    /// for goes now and the rest is kept, in order, for the next write and each pass of
    /// `next_event`, so a harness that stops reading never holds the engine past a stop or the
    /// run clock (T20 sec r1 F1). Input: the bytes. Output: Ok, `launch_input_closed`, or
    /// `launch_input_full` once more than `INPUT_PENDING_MAX` bytes would wait.
    fn write(&mut self, bytes: &[u8]) -> Result<(), LaunchError> {
        if self.stdin.is_none() {
            return Err(failure("launch_input_closed"));
        }
        if self.pending.len() + bytes.len() > INPUT_PENDING_MAX {
            return Err(failure("launch_input_full"));
        }
        self.pending.extend_from_slice(bytes);
        self.flush_input()
    }

    /// Writes as much of the input the harness has not taken yet as its pipe has room for
    /// now, never waiting. Output: Ok, also with bytes still kept or with the input a stop
    /// closed, or `launch_input_closed` once the harness's end of the pipe is gone, the kept
    /// bytes dropped with it, so that call fails and later event passes do not.
    fn flush_input(&mut self) -> Result<(), LaunchError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Ok(());
        };
        while !self.pending.is_empty() {
            match stdin.write(&self.pending) {
                Ok(taken) if taken > 0 => {
                    assert!(taken <= self.pending.len());
                    self.pending.drain(..taken);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Ok(_) | Err(_) => {
                    self.pending.clear();
                    return Err(failure("launch_input_closed"));
                }
            }
        }
        Ok(())
    }

    fn reap(&mut self, wait: Duration) -> Option<i32> {
        let until = Instant::now() + wait;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => return None,
            }
        }
    }

    /// Stops the harness where no process group exists: closes its input, waits up to the
    /// stop grace for it to exit, then kills the direct child and reaps it. Takes the
    /// session. Returns its exit code, or none when already stopped or it has none.
    #[cfg(not(unix))]
    fn stop_group(&mut self) -> Option<i32> {
        if self.stopped {
            return None;
        }
        self.stopped = true;
        self.stdin = None;
        if let Some(warden) = self.warden.take() {
            match warden {}
        }
        let code = self.reap(self.limits.stop_grace);
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        assert!(self.stdin.is_none() && self.warden.is_none());
        code.or_else(|| status.and_then(|status| status.code()))
    }

    #[cfg(unix)]
    fn stop_group(&mut self) -> Option<i32> {
        if self.stopped {
            return None;
        }
        self.stopped = true;
        self.stdin = None;
        let group = rustix::process::Pid::from_child(&self.child);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
        let code = self.reap(self.limits.stop_grace);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        if let Some(mut lifeline) = self.lifeline.take() {
            lifeline.release();
        }
        self.uncertain =
            crate::sandbox::warden::fire_reported(self.warden.take(), "harness").is_err();
        assert!(self.stdin.is_none());
        assert!(self.warden.is_none());
        code.or_else(|| status.and_then(|status| status.code()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_group();
    }
}

struct DurableIo {
    child_input: File,
    input: File,
    child_output: File,
    output: File,
    probe: File,
}

impl DurableIo {
    /// Refuses durable harness input and output where no owner-only FIFO and inherited
    /// lock exist. Takes the state folder, unused. Returns `launch_state_unavailable`.
    #[cfg(not(unix))]
    fn create(_state: &Path) -> Result<Self, LaunchError> {
        Err(failure("launch_state_unavailable"))
    }

    #[cfg(unix)]
    fn create(state: &Path) -> Result<Self, LaunchError> {
        validate_home(state).map_err(|_| failure("launch_state_unsafe"))?;
        let input = state.join(DURABLE_INPUT);
        let output = state.join(DURABLE_OUTPUT);
        for path in [&input, &output] {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(failure("launch_state_in_use")),
            }
        }
        let made = crate::spawn_gate::status(
            Command::new(MKFIFO)
                .arg("-m")
                .arg("600")
                .arg(&input)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .map_err(|_| failure("launch_state_unavailable"))?;
        let fifo =
            std::fs::symlink_metadata(&input).map_err(|_| failure("launch_state_unavailable"))?;
        if !made.success()
            || !fifo.file_type().is_fifo()
            || fifo.uid() != rustix::process::geteuid().as_raw()
            || fifo.mode() & 0o7777 != 0o600
        {
            return Err(failure("launch_state_unsafe"));
        }
        let open = |path: &Path, flags: OFlags, mode: Mode| {
            rustix::fs::open(path, flags | OFlags::NOFOLLOW | OFlags::CLOEXEC, mode)
                .map(File::from)
                .map_err(|_| failure("launch_state_unavailable"))
        };
        let child_input = open(input.as_path(), OFlags::RDWR, Mode::empty())?;
        let writer = open(input.as_path(), OFlags::WRONLY, Mode::empty())?;
        let child_output = open(
            output.as_path(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::APPEND,
            Mode::RUSR | Mode::WUSR,
        )?;
        rustix::fs::flock(&child_output, FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| failure("launch_state_in_use"))?;
        let reader = open(output.as_path(), OFlags::RDWR, Mode::empty())?;
        let probe = open(output.as_path(), OFlags::RDONLY, Mode::empty())?;
        assert!(holder_alive(&probe));
        Ok(Self {
            child_input,
            input: writer,
            child_output,
            output: reader,
            probe,
        })
    }
}

/// Reports whether any process still holds the lock a durable harness took on its output
/// log, which it and its whole process group inherit. Input: a read-only open of the log.
/// Output: true while a holder lives; the probe's own lock is released before returning.
#[cfg(unix)]
pub fn holder_alive(probe: &File) -> bool {
    match rustix::fs::flock(probe, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            let _ = rustix::fs::flock(probe, FlockOperation::Unlock);
            false
        }
        Err(error) => error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN,
    }
}

/// Reports whether any handle still holds an exclusive lock on a durable harness output
/// log, through the standard file lock. Input: a read-only open of the log.
/// Output: true while a holder lives; the probe's own lock is released before returning.
#[cfg(not(unix))]
pub fn holder_alive(probe: &File) -> bool {
    match probe.try_lock() {
        Ok(()) => {
            let _ = probe.unlock();
            false
        }
        Err(error) => matches!(error, std::fs::TryLockError::WouldBlock),
    }
}

/// Empties a durable output log its reader has read to the end once it holds more than
/// `DURABLE_OUTPUT_KEPT_MAX` bytes, and moves the reader back to its start, where the
/// harness's appends now land. Only bytes already read are dropped, unless the harness
/// writes between the length check and the cut, which are microseconds apart. Takes the
/// reader at the log's end. Returns whether it emptied the log.
fn trim_read_output(reader: &mut BufReader<File>) -> bool {
    let Ok(at) = reader.stream_position() else {
        return false;
    };
    if at <= DURABLE_OUTPUT_KEPT_MAX
        || !reader
            .get_ref()
            .metadata()
            .is_ok_and(|log| log.is_file() && log.len() == at)
        || reader.get_ref().set_len(0).is_err()
    {
        return false;
    }
    let rewound = reader.seek(SeekFrom::Start(0));
    assert!(rewound.is_err() || reader.buffer().is_empty());
    rewound.is_ok_and(|start| start == 0)
}

/// Empties a durable output log that nothing reads, such as a reattached agent's, once it
/// holds more than `DURABLE_OUTPUT_KEPT_MAX` bytes, so the log on disk stays bounded while
/// the agent runs on. The path is opened without following a link or waiting on a FIFO and
/// must still name the probed file. Takes the log path and the probe's open of it. Returns
/// whether it emptied the log.
#[cfg(unix)]
pub fn trim_unread_output(output: &Path, probe: &File) -> bool {
    let Ok(probed) = probe.metadata() else {
        return false;
    };
    if probed.len() <= DURABLE_OUTPUT_KEPT_MAX {
        return false;
    }
    let Ok(log) = rustix::fs::open(
        output,
        OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from) else {
        return false;
    };
    let same = log.metadata().is_ok_and(|opened| {
        opened.is_file() && (opened.dev(), opened.ino()) == (probed.dev(), probed.ino())
    });
    same && log.set_len(0).is_ok()
}

/// Makes the engine's own open of a harness's input never wait on the harness: a write takes
/// what the pipe has room for now and reports would-block for the rest. The harness's end of
/// the pipe is an open of its own and stays as it was. Input: the file. Output: Ok, or
/// `launch_stdio_unavailable`.
#[cfg(unix)]
fn never_waits(input: &File) -> Result<(), LaunchError> {
    let unavailable = |_: rustix::io::Errno| failure("launch_stdio_unavailable");
    let flags = rustix::fs::fcntl_getfl(input).map_err(unavailable)?;
    rustix::fs::fcntl_setfl(input, flags | OFlags::NONBLOCK).map_err(unavailable)
}

/// Turns a child's stdin pipe into the file the session writes prompts to.
/// Takes the pipe. Returns a file owning the same descriptor.
#[cfg(unix)]
fn stdin_file(pipe: std::process::ChildStdin) -> File {
    File::from(OwnedFd::from(pipe))
}

/// Turns a child's stdin pipe into the file the session writes prompts to.
/// Takes the pipe. Returns a file owning the same handle.
#[cfg(windows)]
fn stdin_file(pipe: std::process::ChildStdin) -> File {
    File::from(std::os::windows::io::OwnedHandle::from(pipe))
}

/// A run log written up to a byte cap, past which the bytes are dropped, so a harness's
/// output never fills the disk while the harness runs on (T19b 19b-4, rv-secreview note 2).
struct Capped {
    file: File,
    left: u64,
}

impl Capped {
    /// A log that keeps at most `bytes_max` bytes. Inputs: the open file and the cap.
    /// Output: the log.
    fn new(file: File, bytes_max: u64) -> Self {
        Self {
            file,
            left: bytes_max,
        }
    }

    /// Writes as much of `bytes` as the cap leaves room for; a write that fails ends the
    /// log. Input: the bytes. Output: none.
    fn write(&mut self, bytes: &[u8]) {
        let before = self.left;
        let room = before.min(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let Ok(kept) = usize::try_from(room) else {
            return;
        };
        if kept == 0 {
            return;
        }
        self.left = match self.file.write_all(&bytes[..kept]) {
            Ok(()) => before - room,
            Err(_) => 0,
        };
        assert!(self.left < before);
    }
}

/// The files a headless run keeps its harness's raw output in, as a research run on pipes
/// keeps them: its stdout and its stderr, each up to `bytes_max` bytes (T19b 19b-4,
/// QUESTION-0151).
pub struct RunLogs {
    pub events: File,
    pub errors: File,
    pub bytes_max: u64,
}

/// Hands each line of a harness's stdout to the session until the output ends. A line
/// longer than `LINE_BYTES_MAX` is passed over to its newline and reported once as
/// skipped, so no amount of output ends the session. Takes the stdout and the sender.
fn forward(stdout: ChildStdout, sender: SyncSender<Line>) {
    forward_logged(stdout, sender, None);
}

/// [`forward`], with every byte read also going to the run's events log, when it keeps one,
/// up to its cap (T19b 19b-4, QUESTION-0151). Takes the stdout, the sender and the log.
fn forward_logged(stdout: ChildStdout, sender: SyncSender<Line>, mut log: Option<Capped>) {
    let mut reader = BufReader::new(stdout);
    let mut skipping = false;
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(LINE_BYTES_MAX + 1).unwrap_or(u64::MAX);
        match reader.by_ref().take(limit).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => {
                let _ = sender.send(Line::Closed);
                return;
            }
            Ok(_) => {
                if let Some(log) = log.as_mut() {
                    log.write(&line);
                }
                let ended = line.last() == Some(&b'\n');
                if skipping {
                    skipping = !ended;
                    continue;
                }
                if line.len() > LINE_BYTES_MAX && !ended {
                    skipping = true;
                    if sender.send(Line::Skipped).is_err() {
                        return;
                    }
                    continue;
                }
                assert!(line.len() <= LINE_BYTES_MAX + 1);
                if sender.send(Line::Bytes(line)).is_err() {
                    return;
                }
            }
        }
    }
}

/// Hands each line a durable harness writes to its output file to the session until the
/// holder of the file is gone and the file is read to its end. A line longer than
/// `LINE_BYTES_MAX` is passed over to its newline and reported once as skipped. A log read
/// to its end past `DURABLE_OUTPUT_KEPT_MAX` is emptied at a line boundary, so the log on
/// disk stays bounded and the agent runs on. Takes the output file, open for reading and
/// writing, the holder probe and the sender.
fn tail(output: File, probe: File, sender: SyncSender<Line>) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    let mut skipping = false;
    let mut draining = false;
    loop {
        assert!(line.len() <= LINE_BYTES_MAX);
        let limit = u64::try_from(LINE_BYTES_MAX + 1 - line.len()).unwrap_or(1);
        let count = match reader.by_ref().take(limit).read_until(b'\n', &mut line) {
            Ok(count) => count,
            Err(_) => {
                let _ = sender.send(Line::Closed);
                return;
            }
        };
        let ended = line.last() == Some(&b'\n');
        if line.len() > LINE_BYTES_MAX && !ended {
            line.clear();
            if !skipping && sender.send(Line::Skipped).is_err() {
                return;
            }
            skipping = true;
            continue;
        }
        if ended {
            let whole = std::mem::take(&mut line);
            if std::mem::take(&mut skipping) {
                continue;
            }
            if sender.send(Line::Bytes(whole)).is_err() {
                return;
            }
            continue;
        }
        if count > 0 {
            continue;
        }
        if draining {
            if !skipping
                && !line.is_empty()
                && sender.send(Line::Bytes(std::mem::take(&mut line))).is_err()
            {
                return;
            }
            let _ = sender.send(Line::Closed);
            return;
        }
        if holder_alive(&probe) {
            if (line.is_empty() || skipping) && trim_read_output(&mut reader) {
                eprintln!("launch_output_trimmed over={DURABLE_OUTPUT_KEPT_MAX}");
            }
            std::thread::sleep(TAIL_POLL);
        } else {
            draining = true;
        }
    }
}

mod turn_session;
pub use turn_session::TurnSession;

#[cfg(test)]
mod tests;
