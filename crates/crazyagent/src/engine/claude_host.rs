use super::EngineError;
use super::bootstrap::NativeProfileUse;
use crate::adapters::claude::{ClaudeProtocol, ProtocolError};
use crate::adapters::events::ObservedEvent;
use crate::adapters::{NativeAction, NativeHarness};
use crate::protocol::{CredentialRef, FRAME_BYTES_MAX, RequestId};
use crate::routing::ResourceBudget;
use crate::sandbox::InstalledSandbox;
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

mod process;

const ACCOUNT_DEFAULT_MODEL: &str = "native-default";
const INSTRUCTIONS_FLAG: &str = "--append-system-prompt-file";
const INSTRUCTIONS_BYTES_MAX: u64 = 64 * 1024;

#[derive(Clone, Copy)]
pub(in crate::engine) struct ClaudeHostLimits {
    pub(in crate::engine) run_time: Duration,
    pub(in crate::engine) cleanup_time: Duration,
    pub(in crate::engine) stdout_bytes: usize,
    pub(in crate::engine) stderr_bytes: usize,
    pub(in crate::engine) events: usize,
}

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
pub(in crate::engine) fn launch_command(
    executable: &Path,
    project_root: &Path,
    profile_home: &Path,
    model: &str,
    instructions: Option<&Path>,
) -> Result<Command, EngineError> {
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
    command.args([
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--setting-sources",
        "",
        "--settings",
        "{\"disableAllHooks\":true}",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--max-turns",
        "1",
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
    assert_eq!(command.get_current_dir(), Some(project_root));
    assert_eq!(command.get_program(), executable.as_os_str());
    Ok(command)
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Host(&'static str),
    Protocol(ProtocolError),
    Exit(Option<i32>),
}

impl Fault {
    fn error(self) -> EngineError {
        match self {
            Self::Host(code) => failure(code),
            Self::Protocol(error) => EngineError(format!("claude_host_protocol_{error:?}")),
            Self::Exit(code) => EngineError(format!("claude_host_native_exit_{code:?}")),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Starting,
    Running,
    Closing,
    Retired,
}

#[derive(Clone, Copy)]
struct Status {
    phase: Phase,
    fault: Option<Fault>,
}

struct Turn {
    request_id: RequestId,
    text: String,
    reply: oneshot::Sender<Result<(), Fault>>,
}

pub(in crate::engine) struct ClaudeHost {
    turns: SyncSender<Turn>,
    events: mpsc::Receiver<ObservedEvent>,
    status: watch::Receiver<Status>,
    stop: Arc<AtomicBool>,
    close_time: Duration,
    expires: tokio::time::Instant,
}

impl ClaudeHost {
    pub(in crate::engine) async fn spawn(
        command: Command,
        sandbox: InstalledSandbox,
        profile: NativeProfileUse,
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
        let mut host = Self {
            turns,
            events,
            status,
            stop: Arc::clone(&stop),
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
                    protocol,
                    limits,
                    process::Channels {
                        input,
                        output,
                        report,
                        stop,
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

impl Drop for ClaudeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn failure(code: &'static str) -> EngineError {
    EngineError(code.into())
}

fn validate_command(command: &Command, profile: &NativeProfileUse) -> Result<(), Fault> {
    let home = profile
        .profile_path()
        .map_err(|_| Fault::Host("claude_host_profile_changed"))?;
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
    let expected = launch_command(
        Path::new(command.get_program()),
        directory,
        &home,
        model,
        instructions,
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
    Ok(())
}

#[cfg(test)]
#[path = "claude_host/tests.rs"]
mod tests;
