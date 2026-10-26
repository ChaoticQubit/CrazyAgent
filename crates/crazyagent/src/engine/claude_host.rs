use super::EngineError;
use super::bootstrap::NativeProfileUse;
use crate::adapters::NativeAction;
#[cfg(unix)]
use crate::adapters::NativeHarness;
#[cfg(unix)]
use crate::adapters::claude::{ClaudeProtocol, ProtocolError};
use crate::adapters::events::ObservedEvent;
#[cfg(unix)]
use crate::protocol::FRAME_BYTES_MAX;
use crate::protocol::{CredentialRef, RequestId};
use crate::routing::ResourceBudget;
use crate::sandbox::InstalledSandbox;
#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(unix)]
use std::sync::mpsc::{SyncSender, sync_channel};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::sync::{mpsc, oneshot, watch};

#[cfg(unix)]
mod process;

#[cfg(unix)]
const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
#[cfg(unix)]
const INSTRUCTIONS_FLAG: &str = "--append-system-prompt-file";
#[cfg(unix)]
const INSTRUCTIONS_BYTES_MAX: u64 = 64 * 1024;
#[cfg(unix)]
const TOOL_STEPS_MAX: &str = "24";
#[cfg(unix)]
const TOOLS_SHELL: &str = "/bin/zsh";
const NO_SERVERS: &str = "{\"mcpServers\":{}}";
const MCP_CONFIG_FLAG: &str = "--mcp-config";

/// What a Claude Code or Codex master turn with its tools on is given beyond the team agents'
/// tool set: the engine executable that serves its `delegate` tool, and the absolute drop file
/// in the turn's private temporary folder where that tool records each proposed task.
#[derive(Clone, Copy)]
pub(in crate::engine) struct MasterTools<'a> {
    pub(in crate::engine) engine: &'a Path,
    pub(in crate::engine) drop: &'a Path,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(in crate::engine) struct ClaudeHostLimits {
    pub(in crate::engine) run_time: Duration,
    pub(in crate::engine) cleanup_time: Duration,
    pub(in crate::engine) stdout_bytes: usize,
    pub(in crate::engine) stderr_bytes: usize,
    pub(in crate::engine) events: usize,
}

#[cfg(unix)]
impl ClaudeHostLimits {
    fn validate(&self) -> Result<(), EngineError> {
        if self.run_time.is_zero()
            || self.run_time > Duration::from_secs(600)
            || self.cleanup_time.is_zero()
            || self.cleanup_time > Duration::from_secs(5)
            || !(1..=1_048_576).contains(&self.stdout_bytes)
            || !(1..=262_144).contains(&self.stderr_bytes)
            || !(1..=4096).contains(&self.events)
        {
            return Err(failure("claude_host_invalid_limits"));
        }
        Ok(())
    }

    fn from_budget(budget: &ResourceBudget) -> Result<Self, EngineError> {
        budget
            .validate()
            .map_err(|_| failure("claude_host_invalid_budget"))?;
        let limits = Self {
            run_time: Duration::from_millis(budget.wall_time_ms.min(600_000)),
            cleanup_time: Duration::from_secs(5),
            stdout_bytes: usize::try_from(budget.output_bytes.min(1_048_576))
                .map_err(|_| failure("claude_host_invalid_budget"))?,
            stderr_bytes: usize::try_from(budget.output_bytes.min(262_144))
                .map_err(|_| failure("claude_host_invalid_budget"))?,
            events: 4096,
        };
        limits.validate()?;
        assert!(limits.run_time.as_millis() <= u128::from(budget.wall_time_ms));
        assert!(limits.stdout_bytes as u64 <= budget.output_bytes);
        Ok(limits)
    }
}

/// Builds the tools-off Claude Code master command for one turn.
/// Input: the registered executable, the leased project directory, the private profile
/// home, the model and, when given, the engine's 0600 startup instruction file.
/// Output: the command, or a plain refusal for an unsafe path, model or file.
#[cfg(unix)]
pub(in crate::engine) fn launch_command(
    executable: &Path,
    project_root: &Path,
    profile_home: &Path,
    model: &str,
    instructions: Option<&Path>,
) -> Result<Command, EngineError> {
    command_for(
        executable,
        project_root,
        profile_home,
        model,
        instructions,
        None,
    )
}

/// Builds the Claude Code master command for one turn whose tools are on (DEC-093): the team
/// agents' tool flags, a bounded number of steps, hooks and setting sources still off, and
/// only the engine's `delegate` MCP server. Input: as `launch_command`, plus the engine
/// executable and drop file. Output: the command, or a plain refusal for an unsafe input.
#[cfg(unix)]
pub(in crate::engine) fn tools_launch_command(
    executable: &Path,
    project_root: &Path,
    profile_home: &Path,
    model: &str,
    instructions: Option<&Path>,
    tools: MasterTools<'_>,
) -> Result<Command, EngineError> {
    command_for(
        executable,
        project_root,
        profile_home,
        model,
        instructions,
        Some(tools),
    )
}

/// Renders the strict MCP configuration of a tools-on master: one stdio server, the engine
/// executable serving `team-mcp` on the drop file. Input: the tools. Output: the JSON text,
/// or `claude_host_invalid_launch` for a relative path or one with a control character.
fn delegate_config(tools: MasterTools<'_>) -> Result<String, EngineError> {
    let plain = |path: &Path| {
        path.is_absolute()
            .then(|| path.to_str())
            .flatten()
            .filter(|text| !text.chars().any(char::is_control))
            .map(str::to_owned)
    };
    let (Some(engine), Some(drop)) = (plain(tools.engine), plain(tools.drop)) else {
        return Err(failure("claude_host_invalid_launch"));
    };
    let config = serde_json::json!({"mcpServers": {crate::team::tool::SERVER_NAME: {
        "type": "stdio",
        "command": engine,
        "args": [crate::team::tool::SERVER_COMMAND, drop],
    }}});
    Ok(config.to_string())
}

#[cfg(unix)]
fn command_for(
    executable: &Path,
    project_root: &Path,
    profile_home: &Path,
    model: &str,
    instructions: Option<&Path>,
    tools: Option<MasterTools<'_>>,
) -> Result<Command, EngineError> {
    let servers = match tools {
        Some(tools) => delegate_config(tools)?,
        None => NO_SERVERS.to_owned(),
    };
    if !executable.is_absolute()
        || !project_root.is_absolute()
        || !profile_home.is_absolute()
        || model.is_empty()
        || model.len() > 256
        || model.starts_with('-')
        || model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(failure("claude_host_invalid_launch"));
    }
    let project =
        std::fs::metadata(project_root).map_err(|_| failure("claude_host_project_unavailable"))?;
    let home = std::fs::symlink_metadata(profile_home)
        .map_err(|_| failure("claude_host_profile_unavailable"))?;
    if !project.is_dir()
        || !home.is_dir()
        || home.uid() != rustix::process::geteuid().as_raw()
        || home.mode() & 0o7777 != 0o700
    {
        return Err(failure("claude_host_unsafe_directory"));
    }
    if let Some(file) = instructions {
        if !file.is_absolute() {
            return Err(failure("claude_host_unsafe_instructions"));
        }
        let metadata = std::fs::symlink_metadata(file)
            .map_err(|_| failure("claude_host_instructions_unavailable"))?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() > INSTRUCTIONS_BYTES_MAX
        {
            return Err(failure("claude_host_unsafe_instructions"));
        }
    }
    let mut command = Command::new(executable);
    command.args([
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ]);
    if model != ACCOUNT_DEFAULT_MODEL {
        command.args(["--model", model]);
    }
    match tools {
        Some(_) => command.args([
            "--tools",
            "default",
            "--permission-mode",
            "bypassPermissions",
        ]),
        None => command.args(["--tools", ""]),
    };
    command.args([
        "--strict-mcp-config",
        MCP_CONFIG_FLAG,
        servers.as_str(),
        "--setting-sources",
        "",
        "--settings",
        "{\"disableAllHooks\":true}",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--max-turns",
        if tools.is_some() { TOOL_STEPS_MAX } else { "1" },
    ]);
    if let Some(file) = instructions {
        command.arg(INSTRUCTIONS_FLAG).arg(file);
    }
    command
        .current_dir(project_root)
        .env_clear()
        .env("HOME", profile_home)
        .env("CLAUDE_CONFIG_DIR", profile_home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", profile_home)
        .env("TERM", "dumb");
    if tools.is_some() {
        command.env("SHELL", TOOLS_SHELL);
    }
    assert_eq!(command.get_current_dir(), Some(project_root));
    assert_eq!(command.get_program(), executable.as_os_str());
    Ok(command)
}

/// Refuses the Claude Code master command where the profile home's Unix owner and mode
/// cannot be checked. Takes the executable, project, home, model and instruction file,
/// all unused. Returns `claude_host_unavailable`.
#[cfg(not(unix))]
pub(in crate::engine) fn launch_command(
    _executable: &Path,
    _project_root: &Path,
    _profile_home: &Path,
    _model: &str,
    _instructions: Option<&Path>,
) -> Result<Command, EngineError> {
    Err(failure("claude_host_unavailable"))
}

/// Refuses the tools-on Claude Code master command where the profile home's Unix owner and
/// mode cannot be checked. Takes the executable, project, home, model, instruction file and
/// tools, all unused. Returns `claude_host_unavailable`.
#[cfg(not(unix))]
pub(in crate::engine) fn tools_launch_command(
    _executable: &Path,
    _project_root: &Path,
    _profile_home: &Path,
    _model: &str,
    _instructions: Option<&Path>,
    _tools: MasterTools<'_>,
) -> Result<Command, EngineError> {
    Err(failure("claude_host_unavailable"))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Fault {
    Host(&'static str),
    Protocol(ProtocolError),
    Exit(Option<i32>),
}

#[cfg(unix)]
impl Fault {
    fn error(self) -> EngineError {
        match self {
            Self::Host(code) => failure(code),
            Self::Protocol(error) => protocol_error(error),
            Self::Exit(Some(code)) => EngineError(format!("claude_host_native_exit_{code}")),
            Self::Exit(None) => failure("claude_host_native_exit_signal"),
        }
    }
}

/// Names a Claude protocol fault as a plain lowercase code that engine logs can show.
/// Input: the protocol error. Output: `claude_host_protocol_` followed by the error's
/// name in snake case, for example `claude_host_protocol_stale_message`.
#[cfg(unix)]
fn protocol_error(error: ProtocolError) -> EngineError {
    let name = format!("{error:?}");
    let mut code = String::from("claude_host_protocol");
    for character in name.chars() {
        if character.is_ascii_uppercase() {
            code.push('_');
        }
        code.push(character.to_ascii_lowercase());
    }
    EngineError(code)
}

#[cfg(unix)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Starting,
    Running,
    Closing,
    Retired,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct Status {
    phase: Phase,
    fault: Option<Fault>,
}

#[cfg(unix)]
struct Turn {
    request_id: RequestId,
    text: String,
    reply: oneshot::Sender<Result<(), Fault>>,
}

#[cfg(unix)]
pub(in crate::engine) struct ClaudeHost {
    turns: SyncSender<Turn>,
    events: mpsc::Receiver<ObservedEvent>,
    status: watch::Receiver<Status>,
    stop: Arc<AtomicBool>,
    group: Arc<AtomicU32>,
    close_time: Duration,
    expires: tokio::time::Instant,
}

#[cfg(unix)]
impl ClaudeHost {
    pub(in crate::engine) async fn spawn(
        command: Command,
        sandbox: InstalledSandbox,
        profile: NativeProfileUse,
        home: PathBuf,
        action: NativeAction,
        account_ref: CredentialRef,
        budget: ResourceBudget,
    ) -> Result<Self, EngineError> {
        let limits = ClaudeHostLimits::from_budget(&budget)?;
        let protocol = ClaudeProtocol::new(action, account_ref)
            .map_err(|error| Fault::Protocol(error).error())?;
        let (turns, input) = sync_channel(1);
        let (output, events) = mpsc::channel(128);
        let (report, status) = watch::channel(Status {
            phase: Phase::Starting,
            fault: None,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let group = Arc::new(AtomicU32::new(0));
        let mut host = Self {
            turns,
            events,
            status,
            stop: Arc::clone(&stop),
            group: Arc::clone(&group),
            close_time: limits.cleanup_time,
            expires: tokio::time::Instant::now() + limits.run_time,
        };
        let _custodian = std::thread::Builder::new()
            .name("claude-master-custodian".into())
            .spawn(move || {
                process::run(
                    command,
                    sandbox,
                    profile,
                    home,
                    protocol,
                    limits,
                    process::Channels {
                        input,
                        output,
                        report,
                        stop,
                        group,
                    },
                );
            })
            .map_err(|_| failure("claude_host_custodian_unavailable"))?;
        loop {
            let current = *host.status.borrow();
            if let Some(error) = current.fault {
                return Err(error.error());
            }
            if current.phase == Phase::Running {
                return Ok(host);
            }
            if current.phase == Phase::Retired {
                return Err(failure("claude_host_disconnected"));
            }
            tokio::time::timeout_at(host.expires, host.status.changed())
                .await
                .map_err(|_| failure("claude_host_startup_timeout"))?
                .map_err(|_| failure("claude_host_custodian_lost"))?;
        }
    }

    pub(in crate::engine) async fn start_turn(
        &mut self,
        request_id: RequestId,
        text: &str,
    ) -> Result<(), EngineError> {
        if self.stop.load(Ordering::Acquire)
            || self.status.borrow().phase != Phase::Running
            || text.is_empty()
            || text.len() > FRAME_BYTES_MAX
        {
            return Err(failure("claude_host_turn_unavailable"));
        }
        let (reply, mut response) = oneshot::channel();
        self.turns
            .try_send(Turn {
                request_id,
                text: text.to_owned(),
                reply,
            })
            .map_err(|_| failure("claude_host_input_unavailable"))?;
        loop {
            let current = *self.status.borrow();
            if current.phase != Phase::Running {
                return Err(current
                    .fault
                    .map_or_else(|| failure("claude_host_input_closed"), Fault::error));
            }
            tokio::select! {
                result = &mut response => {
                    return result.map_err(|_| failure("claude_host_input_disconnected"))?
                        .map_err(Fault::error);
                }
                changed = self.status.changed() => {
                    changed.map_err(|_| failure("claude_host_custodian_lost"))?;
                }
                _ = tokio::time::sleep_until(self.expires) => {
                    self.stop.store(true, Ordering::Release);
                    return Err(failure("claude_host_deadline"));
                }
            }
        }
    }

    pub(in crate::engine) async fn next_event(&mut self) -> Result<ObservedEvent, EngineError> {
        loop {
            if let Ok(event) = self.events.try_recv() {
                assert_eq!(event.provenance.harness, NativeHarness::Claude);
                assert!(event.native_session_id.is_some());
                return Ok(event);
            }
            let current = *self.status.borrow();
            if let Some(fault) = current.fault {
                return Err(fault.error());
            }
            if current.phase == Phase::Retired {
                return Err(failure("claude_host_disconnected"));
            }
            tokio::select! {
                event = self.events.recv() => {
                    let event = event.ok_or_else(|| failure("claude_host_disconnected"))?;
                    assert_eq!(event.provenance.harness, NativeHarness::Claude);
                    assert!(event.native_session_id.is_some());
                    return Ok(event);
                }
                changed = self.status.changed() => {
                    changed.map_err(|_| failure("claude_host_custodian_lost"))?;
                }
                _ = tokio::time::sleep_until(self.expires) => {
                    self.stop.store(true, Ordering::Release);
                    return Err(failure("claude_host_deadline"));
                }
            }
        }
    }

    /// Names the process group the harness was started in, which can outlive this host when
    /// the engine stops. Input: the host. Output: the group, or none before the harness
    /// started.
    pub(in crate::engine) fn process_group(&self) -> Option<u32> {
        Some(self.group.load(Ordering::Acquire)).filter(|group| *group > 1)
    }

    pub(in crate::engine) async fn cancel(&mut self) -> Result<(), EngineError> {
        self.stop.store(true, Ordering::Release);
        self.close().await
    }

    pub(in crate::engine) async fn close(&mut self) -> Result<(), EngineError> {
        self.stop.store(true, Ordering::Release);
        let deadline = tokio::time::Instant::now() + self.close_time;
        loop {
            let current = *self.status.borrow();
            if current.phase == Phase::Retired {
                return current.fault.map_or(Ok(()), |error| Err(error.error()));
            }
            tokio::time::timeout_at(deadline, self.status.changed())
                .await
                .map_err(|_| failure("claude_host_cleanup_uncertain"))?
                .map_err(|_| failure("claude_host_custodian_lost"))?;
        }
    }
}

#[cfg(unix)]
impl Drop for ClaudeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Stands in for the Claude Code master host where native profile custody is unavailable.
/// It has no values: `spawn` needs a native profile lease, which cannot exist here, so no
/// turn, event, cancel or close call can be reached.
#[cfg(not(unix))]
pub(in crate::engine) enum ClaudeHost {}

#[cfg(not(unix))]
impl ClaudeHost {
    /// Never starts: takes the command, boundary, uninhabited profile lease, turn home,
    /// action, account and budget. Returns nothing because the lease cannot exist.
    pub(in crate::engine) async fn spawn(
        _command: Command,
        _sandbox: InstalledSandbox,
        profile: NativeProfileUse,
        _home: PathBuf,
        _action: NativeAction,
        _account_ref: CredentialRef,
        _budget: ResourceBudget,
    ) -> Result<Self, EngineError> {
        match profile {}
    }

    /// Never sends a turn: takes the request and text. Unreachable without a host value.
    pub(in crate::engine) async fn start_turn(
        &mut self,
        _request_id: RequestId,
        _text: &str,
    ) -> Result<(), EngineError> {
        match *self {}
    }

    /// Never yields an event: takes the host. Unreachable without a host value.
    pub(in crate::engine) async fn next_event(&mut self) -> Result<ObservedEvent, EngineError> {
        match *self {}
    }

    /// Never names a group: takes the host. Unreachable without a host value.
    pub(in crate::engine) fn process_group(&self) -> Option<u32> {
        match *self {}
    }

    /// Never cancels: takes the host. Unreachable without a host value.
    pub(in crate::engine) async fn cancel(&mut self) -> Result<(), EngineError> {
        match *self {}
    }

    /// Never closes: takes the host. Unreachable without a host value.
    pub(in crate::engine) async fn close(&mut self) -> Result<(), EngineError> {
        match *self {}
    }
}

fn failure(code: &'static str) -> EngineError {
    EngineError(code.into())
}

/// Reads the delegate server a tools-on master command names, so the command can be built
/// again from it and compared. Input: the `--mcp-config` text. Output: the engine executable
/// and drop file, or none when the text is not one engine server.
#[cfg(unix)]
fn named_tools(config: &str) -> Option<(PathBuf, PathBuf)> {
    let config: serde_json::Value = serde_json::from_str(config).ok()?;
    let server = config
        .get("mcpServers")?
        .get(crate::team::tool::SERVER_NAME)?;
    let engine = server.get("command")?.as_str()?;
    let drop = server.get("args")?.as_array()?.get(1)?.as_str()?;
    Some((PathBuf::from(engine), PathBuf::from(drop)))
}

/// Confirms a Claude Code master command names exactly the delegate server the engine gave
/// its turn: with `tools`, one MCP configuration whose only server is that engine executable
/// serving `team-mcp` on that drop file; without, an empty server list. Input: the command
/// and the expected tools. Output: Ok, or `master_delegate_unverified`.
pub(in crate::engine) fn require_delegate(
    command: &Command,
    tools: Option<MasterTools<'_>>,
) -> Result<(), EngineError> {
    let refused = || failure("master_delegate_unverified");
    let expected = match tools {
        Some(tools) => delegate_config(tools).map_err(|_| refused())?,
        None => NO_SERVERS.to_owned(),
    };
    let expected: serde_json::Value = serde_json::from_str(&expected).map_err(|_| refused())?;
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    let inline = format!("{MCP_CONFIG_FLAG}=");
    if args
        .iter()
        .any(|arg| arg.as_encoded_bytes().starts_with(inline.as_bytes()))
    {
        return Err(refused());
    }
    let mut configs = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| **arg == std::ffi::OsStr::new(MCP_CONFIG_FLAG))
        .map(|(index, _)| args.get(index + 1).and_then(|config| config.to_str()));
    let (Some(Some(config)), None) = (configs.next(), configs.next()) else {
        return Err(refused());
    };
    let named: serde_json::Value = serde_json::from_str(config).map_err(|_| refused())?;
    if named != expected {
        return Err(refused());
    }
    Ok(())
}

/// Confirms a master command is exactly one the engine builds: tools off, or tools on with
/// only the engine's delegate server, for this directory, home, model and instruction file.
/// Input: the command, the profile lease and the turn home. Output: whether the tools are
/// on, or the fault that refuses the command.
#[cfg(unix)]
fn validate_command(
    command: &Command,
    profile: &NativeProfileUse,
    home: &Path,
) -> Result<bool, Fault> {
    let directory = command
        .get_current_dir()
        .ok_or(Fault::Host("claude_host_project_unavailable"))?;
    let args: Vec<_> = command.get_args().collect();
    let model = match args.iter().position(|arg| *arg == OsStr::new("--model")) {
        Some(index) => args
            .get(index + 1)
            .and_then(|model| model.to_str())
            .ok_or(Fault::Host("claude_host_model_unavailable"))?,
        None => ACCOUNT_DEFAULT_MODEL,
    };
    let instructions = match args
        .iter()
        .position(|arg| *arg == OsStr::new(INSTRUCTIONS_FLAG))
    {
        Some(index) => Some(Path::new(
            args.get(index + 1)
                .ok_or(Fault::Host("claude_host_instructions_unavailable"))?,
        )),
        None => None,
    };
    let tools = args.contains(&OsStr::new("--permission-mode"));
    let named = if tools {
        let named = args
            .iter()
            .position(|arg| *arg == OsStr::new(MCP_CONFIG_FLAG))
            .and_then(|index| args.get(index + 1))
            .and_then(|config| config.to_str())
            .and_then(named_tools)
            .ok_or(Fault::Host("claude_host_invalid_command"))?;
        Some(named)
    } else {
        None
    };
    let expected = command_for(
        Path::new(command.get_program()),
        directory,
        home,
        model,
        instructions,
        named
            .as_ref()
            .map(|(engine, drop)| MasterTools { engine, drop }),
    )
    .map_err(|_| Fault::Host("claude_host_invalid_command"))?;
    if command.get_args().ne(expected.get_args()) || command.get_envs().ne(expected.get_envs()) {
        return Err(Fault::Host("claude_host_unapproved_command"));
    }
    profile
        .revalidate_retained()
        .map_err(|_| Fault::Host("claude_host_profile_changed"))?;
    assert!(directory.is_absolute());
    assert_eq!(args.len(), expected.get_args().count());
    assert_eq!(tools, named.is_some());
    Ok(tools)
}

#[cfg(all(test, unix))]
#[path = "claude_host/tests.rs"]
mod tests;
