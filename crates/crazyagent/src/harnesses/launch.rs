use crate::adapters::claude::{ClaudeProtocol, State as ClaudeState};
use crate::adapters::codex::{CodexProtocol, Event as CodexEvent, State as CodexState, TurnStatus};
use crate::adapters::events::{EventKind, RateLimitStatus, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::credentials::SecretString;
use crate::launch_context::{AgentContext, Installed};
use crate::model_gateway::relay::{Relay, is_placeholder};
use crate::protocol::{CredentialRef, RequestId, SessionId};
#[cfg(unix)]
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::Serialize;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

pub(super) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const LINE_BYTES_MAX: usize = crate::adapters::codex::FRAME_BYTES_MAX;
pub(super) const TOOLS_OFF_PATH: &str = "/usr/bin:/bin";
pub(super) const TOOLS_ON_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
pub(super) const CODEX_KEY_VARIABLE: &str = "CRAZYAGENT_API_KEY";
pub(super) const LOCAL_PLACEHOLDER_KEY: &str = crate::model_gateway::relay::LOCAL_PLACEHOLDER;
const CODEX_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const CLAUDE_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DURABLE_INPUT: &str = "input";
pub const DURABLE_OUTPUT: &str = "output.log";
#[cfg(unix)]
const MKFIFO: &str = "/usr/bin/mkfifo";
const TAIL_POLL: Duration = Duration::from_millis(20);
const CODEX_TOOLS_OFF: [&str; 13] = [
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
const CODEX_TOOLS_ON: [&str; 6] = [
    "sandbox_mode=\"danger-full-access\"",
    "approval_policy=\"never\"",
    "features.apps=false",
    "features.plugins=false",
    "features.goals=false",
    "mcp_servers={}",
];

type Invocation = (Vec<OsString>, Vec<(&'static str, OsString)>);

pub(super) const CLAUDE_NO_MCP: [&str; 2] = ["--mcp-config", "{\"mcpServers\":{}}"];
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
    "{\"disableAllHooks\":true}",
    "--no-session-persistence",
];

/// Holds an API key or token in memory for one launch; it never appears in Debug,
/// serialization, arguments or reports and is handed to the child only as an environment value.
pub struct Secret(String);

impl Secret {
    /// Wraps a key after bounds checks. Input: the key text. Output: the secret, or
    /// `launch_secret_invalid` when it is empty, oversized or contains whitespace or controls.
    pub fn new(value: String) -> Result<Self, LaunchError> {
        if value.is_empty()
            || value.len() > 4096
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
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

/// Bounds for one launched harness process.
/// `run_time` caps the whole session, `stop_grace` the wait between TERM and KILL,
/// `output_bytes` the total stdout read and `events` the events delivered.
#[derive(Clone, Copy, Debug)]
pub struct LaunchLimits {
    pub run_time: Duration,
    pub stop_grace: Duration,
    pub output_bytes: usize,
    pub events: usize,
}

impl Default for LaunchLimits {
    fn default() -> Self {
        Self {
            run_time: Duration::from_secs(600),
            stop_grace: Duration::from_secs(2),
            output_bytes: 8 * 1024 * 1024,
            events: 10_000,
        }
    }
}

/// Everything needed to start one harness: catalog id, executable, working directory,
/// account, optional model, tools switch, the project sandbox profile and limits.
/// Every launch is refused unless a sandbox profile is given.
#[derive(Debug)]
pub struct LaunchRequest {
    pub harness: String,
    pub executable: PathBuf,
    pub working_dir: PathBuf,
    pub account: AccountBinding,
    pub model: Option<String>,
    pub tools: bool,
    pub sandbox_profile: Option<PathBuf>,
    pub limits: LaunchLimits,
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
    TurnError { rate_limited: bool },
    Exited { code: Option<i32> },
}

/// Tells whether a provider error message says the account hit a rate or usage limit.
/// Takes the provider's error text, which is never stored or shown.
/// Returns true for rate limit, usage limit, quota and HTTP 429 wording.
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
}

/// A plain failure code from a launch adapter. It never carries secrets or raw output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchError(pub String);

pub(super) fn failure(code: &str) -> LaunchError {
    LaunchError(code.into())
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
    let default = match harness_id(&request) {
        "codex" => CODEX_DEFAULT_BASE_URL,
        "claude-code" => CLAUDE_DEFAULT_BASE_URL,
        _ => return Err(failure("launch_key_relay_unsupported")),
    };
    let upstream = request.account.base_url().unwrap_or(default).to_owned();
    let secret = SecretString::new(key.reveal().as_bytes().to_vec())
        .map_err(|_| failure("launch_secret_invalid"))?;
    let relay = Relay::start(&upstream, secret).map_err(|error| LaunchError(error.0))?;
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

/// Builds the exact command a launch would spawn, without spawning it.
/// Input: the launch request. Output: the command, or a plain refusal code when an
/// input is unsafe or the harness has no launch adapter.
pub fn command(request: &LaunchRequest) -> Result<Command, LaunchError> {
    build(request, None)
}

/// Builds the command for an agent of one project: writes the project's approved skills,
/// MCP servers, brief, context index and approved memories into the agent home, then
/// points the harness at them. Input: the launch request and the project's context from
/// `launch_context::for_project`. Output: the command and the guard that removes the
/// generated files when dropped; no MCP secret is ever in an argument or the environment.
pub fn command_with_context(
    request: &LaunchRequest,
    context: &AgentContext,
) -> Result<(Command, Installed), LaunchError> {
    validate(request)?;
    let installed = context.install(&request.harness, request.account.home(), request.tools)?;
    let command = build(request, Some(&installed))?;
    assert!(
        command
            .get_args()
            .all(|argument| !context.exposes_secret(argument.as_encoded_bytes()))
    );
    assert!(command.get_envs().all(|(name, value)| {
        !context.exposes_secret(name.as_encoded_bytes())
            && value.is_none_or(|value| !context.exposes_secret(value.as_encoded_bytes()))
    }));
    Ok((command, installed))
}

fn build(request: &LaunchRequest, installed: Option<&Installed>) -> Result<Command, LaunchError> {
    validate(request)?;
    let home = request.account.home();
    let (arguments, environment) = match harness_id(request) {
        "codex" => codex_invocation(request, installed.is_some())?,
        "claude-code" => claude_invocation(request, installed),
        _ => return Err(failure("launch_harness_unsupported")),
    };
    let profile = request
        .sandbox_profile
        .as_ref()
        .ok_or_else(|| failure("launch_sandbox_required"))?;
    if !cfg!(target_os = "macos") {
        return Err(failure("launch_sandbox_unavailable"));
    }
    let path = if request.tools {
        TOOLS_ON_PATH
    } else {
        TOOLS_OFF_PATH
    };
    let mut command = crate::sandbox::agent_command(
        profile,
        &request.executable,
        &[
            ("HOME", home.as_os_str()),
            ("PATH", OsStr::new(path)),
            ("LANG", OsStr::new("C")),
            ("LC_ALL", OsStr::new("C")),
        ],
        environment
            .iter()
            .map(|(name, value)| (OsStr::new(*name), Some(value.as_os_str()))),
    )
    .map_err(|_| failure("launch_environment_refused"))?;
    command
        .args(&arguments)
        .current_dir(&request.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_eq!(
        command.get_current_dir(),
        Some(request.working_dir.as_path())
    );
    assert_eq!(command.get_program(), SANDBOX_EXEC);
    assert!(command.get_args().all(|argument| {
        request
            .account
            .key()
            .is_none_or(|key| !argument.to_string_lossy().contains(&key.0))
    }));
    Ok(command)
}

fn codex_invocation(request: &LaunchRequest, context: bool) -> Result<Invocation, LaunchError> {
    let home = request.account.home().as_os_str().to_owned();
    let mut environment = vec![("CODEX_HOME", home.clone()), ("TMPDIR", home)];
    if let Some(key) = request.account.key() {
        environment.push((CODEX_KEY_VARIABLE, OsString::from(&key.0)));
    }
    let mut arguments = Vec::new();
    for setting in codex_configuration(request, context) {
        arguments.push(OsString::from("-c"));
        arguments.push(OsString::from(setting));
    }
    arguments.extend(["app-server", "--listen", "stdio://"].map(OsString::from));
    Ok((arguments, environment))
}

/// Builds the `-c` settings every Codex launch gets: file credentials in the account home,
/// the account's provider, the model and the tools switch. Input: the request and whether
/// project context was installed. Output: the settings, in order, without any secret.
pub(super) fn codex_configuration(request: &LaunchRequest, context: bool) -> Vec<String> {
    let mut configuration = vec!["cli_auth_credentials_store=\"file\"".to_owned()];
    match &request.account {
        AccountBinding::Subscription { .. } => {
            configuration.push("model_provider=\"openai\"".into());
        }
        AccountBinding::ApiKey { .. } | AccountBinding::Url { .. } => {
            let base_url = request.account.base_url().unwrap_or(CODEX_DEFAULT_BASE_URL);
            let key = request.account.key().map_or(String::new(), |_| {
                format!(",env_key=\"{CODEX_KEY_VARIABLE}\"")
            });
            configuration.push(format!(
                "model_providers.crazyagent={{name=\"CrazyAgent\",base_url=\"{base_url}\",wire_api=\"responses\"{key}}}"
            ));
            configuration.push("model_provider=\"crazyagent\"".into());
        }
    }
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

fn claude_invocation(request: &LaunchRequest, installed: Option<&Installed>) -> Invocation {
    let home = request.account.home().as_os_str().to_owned();
    let mut arguments: Vec<OsString> = CLAUDE_COMMON.iter().map(OsString::from).collect();
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
        arguments.extend(["--tools", "", "--disable-slash-commands"].map(OsString::from));
    }
    let mut environment = vec![
        ("CLAUDE_CONFIG_DIR", home.clone()),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR", home),
        ("TERM", OsString::from("dumb")),
        ("DISABLE_TELEMETRY", OsString::from("1")),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            OsString::from("1"),
        ),
    ];
    if request.tools {
        environment.push(("SHELL", OsString::from("/bin/zsh")));
    }
    if !matches!(request.account, AccountBinding::Subscription { .. }) {
        arguments.push(OsString::from("--bare"));
        let key = request
            .account
            .key()
            .map_or(LOCAL_PLACEHOLDER_KEY, |key| key.0.as_str());
        environment.push(("ANTHROPIC_API_KEY", OsString::from(key)));
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
    match &request.sandbox_profile {
        Some(profile) if !profile.is_absolute() || !profile.is_file() => {
            return Err(failure("launch_sandbox_profile_unavailable"));
        }
        Some(_) => {}
        None => return Err(failure("launch_sandbox_required")),
    }
    let limits = request.limits;
    if limits.run_time.is_zero()
        || limits.run_time > Duration::from_secs(86_400)
        || limits.stop_grace > Duration::from_secs(30)
        || !(1..=64 * 1024 * 1024).contains(&limits.output_bytes)
        || !(1..=1_000_000).contains(&limits.events)
    {
        return Err(failure("launch_limits_invalid"));
    }
    Ok(())
}

/// Checks that an account home is safe to launch in: an absolute 0700 directory owned by
/// this user that is neither the owner's HOME nor their real `~/.codex` or `~/.claude`.
/// Input: the home. Output: Ok, or a plain `launch_account_home_*` refusal code.
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
        for forbidden in [owner.clone(), owner.join(".codex"), owner.join(".claude")] {
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

enum Line {
    Bytes(Vec<u8>),
    Closed,
    Overflow,
}

enum Driver {
    Codex {
        protocol: CodexProtocol,
        cwd: PathBuf,
        tools: bool,
        prompt: Option<String>,
        interrupt: bool,
        server_turn: bool,
    },
    Claude {
        protocol: ClaudeProtocol,
        turns: u64,
        interrupt: Option<String>,
    },
}

impl Driver {
    fn new(request: &LaunchRequest) -> Result<(Self, Vec<Vec<u8>>), LaunchError> {
        match harness_id(request) {
            "codex" => {
                let mut protocol = CodexProtocol::new();
                let start = protocol
                    .initialize()
                    .map_err(|_| failure("launch_protocol_failed"))?;
                let driver = Self::Codex {
                    protocol,
                    cwd: request.working_dir.clone(),
                    tools: request.tools,
                    prompt: None,
                    interrupt: false,
                    server_turn: false,
                };
                Ok((driver, vec![start]))
            }
            "claude-code" => {
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
                        protocol,
                        turns: 0,
                        interrupt: None,
                    },
                    Vec::new(),
                ))
            }
            _ => Err(failure("launch_harness_unsupported")),
        }
    }

    fn send(&mut self, text: &str) -> Result<Option<Vec<u8>>, LaunchError> {
        if text.trim().is_empty() || text.contains('\0') || text.len() > LINE_BYTES_MAX / 2 {
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
            } => {
                let method: serde_json::Value =
                    serde_json::from_slice(line).map_err(|_| failure("launch_protocol_failed"))?;
                let started = method.get("method").and_then(serde_json::Value::as_str)
                    == Some("turn/started");
                let decoded = protocol
                    .receive(&framed)
                    .map_err(|_| failure("launch_protocol_failed"))?;
                if started {
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
                ..
            } => {
                let value: serde_json::Value =
                    serde_json::from_slice(line).map_err(|_| failure("launch_protocol_failed"))?;
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
                                EventKind::TurnFinished { outcome, .. } => {
                                    *interrupt = None;
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
        }
        Ok((events, writes))
    }
}

/// A running harness process driven through its headless interface.
/// The process leads its own process group so stop reaches every descendant.
pub struct Session {
    child: Child,
    stdin: Option<File>,
    lines: Receiver<Line>,
    driver: Driver,
    queue: VecDeque<LaunchEvent>,
    limits: LaunchLimits,
    deadline: Instant,
    events: usize,
    exited: bool,
    stopped: bool,
    _context: Option<Installed>,
    _relay: Option<Relay>,
}

impl Session {
    /// Spawns the harness for a request and begins its handshake; a real provider key goes
    /// through [`route`]. Input: the launch request. Output: a running session, or a plain
    /// refusal code.
    pub fn start(request: LaunchRequest) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let command = command(&request)?;
        Self::spawn(request, command, None, None, relay)
    }

    /// Spawns the harness for an agent of one project with the project's approved skills,
    /// MCP servers, brief, context index and approved memories, the way every engine launch
    /// for a project should. Input: the launch request and `launch_context::for_project`.
    /// Output: a running session; its generated files are removed when it stops.
    pub fn start_with_context(
        request: LaunchRequest,
        context: AgentContext,
    ) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let (command, installed) = command_with_context(&request, &context)?;
        drop(context);
        Self::spawn(request, command, Some(installed), None, relay)
    }

    /// Spawns a harness that outlives the engine: its input is a FIFO it holds open for
    /// reading and writing, so it never sees end of input when the engine dies, and its
    /// output goes to a private log whose lock it holds for as long as any process of its
    /// group lives. Input: the request, the project context for a project agent and the
    /// agent's empty 0700 state directory. Output: a running session, or a plain refusal.
    pub fn start_durable(
        request: LaunchRequest,
        context: Option<AgentContext>,
        state: &Path,
    ) -> Result<Self, LaunchError> {
        let (request, relay) = route(request)?;
        let (command, installed) = match &context {
            Some(context) => {
                let (command, installed) = command_with_context(&request, context)?;
                (command, Some(installed))
            }
            None => (command(&request)?, None),
        };
        drop(context);
        let io = DurableIo::create(state)?;
        Self::spawn(request, command, installed, Some(io), relay)
    }

    fn spawn(
        request: LaunchRequest,
        mut command: Command,
        context: Option<Installed>,
        durable: Option<DurableIo>,
        relay: Option<Relay>,
    ) -> Result<Self, LaunchError> {
        let (driver, startup) = Driver::new(&request)?;
        let output_bytes = request.limits.output_bytes;
        let (sender, lines) = channel();
        let mut child;
        let stdin;
        match durable {
            Some(io) => {
                command
                    .stdin(Stdio::from(io.child_input))
                    .stdout(Stdio::from(io.child_output))
                    .stderr(Stdio::null());
                child = command
                    .spawn()
                    .map_err(|_| failure("launch_spawn_failed"))?;
                drop(command);
                stdin = Some(io.input);
                let (output, probe) = (io.output, io.probe);
                std::thread::Builder::new()
                    .name("harness-output".into())
                    .spawn(move || tail(output, probe, sender, output_bytes))
                    .map_err(|_| failure("launch_reader_unavailable"))?;
            }
            None => {
                child = command
                    .spawn()
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
                std::thread::Builder::new()
                    .name("harness-stdout".into())
                    .spawn(move || forward(stdout, sender, output_bytes))
                    .map_err(|_| failure("launch_reader_unavailable"))?;
                std::thread::Builder::new()
                    .name("harness-stderr".into())
                    .spawn(move || {
                        let mut stderr = stderr;
                        let mut buffer = [0_u8; 4096];
                        while matches!(stderr.read(&mut buffer), Ok(count) if count > 0) {}
                    })
                    .map_err(|_| failure("launch_reader_unavailable"))?;
            }
        }
        let mut session = Self {
            child,
            stdin,
            lines,
            driver,
            queue: VecDeque::new(),
            limits: request.limits,
            deadline: Instant::now() + request.limits.run_time,
            events: 0,
            exited: false,
            stopped: false,
            _context: context,
            _relay: relay,
        };
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

    /// Waits up to `wait` for the next event. Output: Some(event), None on timeout,
    /// or an error once the session deadline passes or the output is malformed or too large.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        let until = Instant::now() + wait;
        loop {
            if let Some(event) = self.queue.pop_front() {
                self.events += 1;
                if self.events > self.limits.events {
                    self.stop_group();
                    return Err(failure("launch_event_limit"));
                }
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
            if now >= until {
                return Ok(None);
            }
            match self.lines.recv_timeout(until.min(self.deadline) - now) {
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
                Ok(Line::Overflow) => {
                    self.stop_group();
                    return Err(failure("launch_output_limit"));
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

    /// Stops the whole process group: TERM, a bounded grace wait, then KILL.
    /// Output: the leader's exit code when it exited normally.
    pub fn stop(mut self) -> Option<i32> {
        self.stop_group()
    }

    fn require_live(&self) -> Result<(), LaunchError> {
        if self.exited || self.stopped {
            return Err(failure("launch_closed"));
        }
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), LaunchError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| failure("launch_input_closed"))?;
        stdin
            .write_all(bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| failure("launch_input_closed"))
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
        let code = self.reap(self.limits.stop_grace);
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        assert!(self.stdin.is_none());
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
        assert!(self.stdin.is_none());
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
        let made = Command::new(MKFIFO)
            .arg("-m")
            .arg("600")
            .arg(&input)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
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
        let reader = open(output.as_path(), OFlags::RDONLY, Mode::empty())?;
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

fn forward(stdout: ChildStdout, sender: Sender<Line>, output_bytes: usize) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0_usize;
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(LINE_BYTES_MAX + 1).unwrap_or(u64::MAX);
        match reader.by_ref().take(limit).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => {
                let _ = sender.send(Line::Closed);
                return;
            }
            Ok(count) => {
                total = total.saturating_add(count);
                if total > output_bytes
                    || (line.len() > LINE_BYTES_MAX && line.last() != Some(&b'\n'))
                {
                    let _ = sender.send(Line::Overflow);
                    return;
                }
                if sender.send(Line::Bytes(line)).is_err() {
                    return;
                }
            }
        }
    }
}

fn tail(output: File, probe: File, sender: Sender<Line>, output_bytes: usize) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    let mut total = 0_usize;
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
        total = total.saturating_add(count);
        if total > output_bytes || (line.len() > LINE_BYTES_MAX && line.last() != Some(&b'\n')) {
            let _ = sender.send(Line::Overflow);
            return;
        }
        if line.last() == Some(&b'\n') {
            if sender.send(Line::Bytes(std::mem::take(&mut line))).is_err() {
                return;
            }
            continue;
        }
        if count > 0 {
            continue;
        }
        if draining {
            if !line.is_empty() && sender.send(Line::Bytes(std::mem::take(&mut line))).is_err() {
                return;
            }
            let _ = sender.send(Line::Closed);
            return;
        }
        if holder_alive(&probe) {
            std::thread::sleep(TAIL_POLL);
        } else {
            draining = true;
        }
    }
}

#[cfg(test)]
mod tests;
